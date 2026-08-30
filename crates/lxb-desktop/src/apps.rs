//! Application discovery: XDG desktop entries, grouped into Plasma-style
//! categories.
//!
//! The `.desktop` format is a small INI dialect, and the parts we need (the
//! `Desktop Entry` group, localised names, `Exec` field codes) are stable and
//! well specified, so it is parsed here rather than pulled in as a dependency.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::theme::Color;
use lxb_protocol::wallpaper::Style;

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
    /// `Keywords`: the words a search would find this entry by. Read for two
    /// questions and no others — whether this is a store, and whether the
    /// entry has asked to be drawn in the shell's own material — because those
    /// are the two things an author can say about an application that its
    /// categories have no vocabulary for. See [`is_store`] and
    /// [`App::wears_shell_material`].
    pub keywords: Vec<String>,
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
/// A column is a tree rather than a list, as the original console shells'
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
    /// The field at the head of a long column — a shelf of the user's own
    /// files, a folder, a Steam library — and the row that empties it.
    ///
    /// A row rather than a control drawn over the column, because on this bar
    /// a row is the only thing there is. The user reaches it by pressing Up
    /// from the first file, presses it with the same button that opens a file,
    /// and it sits where anything standing over a list sits — at the top of it.
    /// Nothing new had to be learnt to find it.
    ///
    /// One row for all three, so it is also nothing new to learn the second
    /// time: what differs between them is only where the narrowing happens,
    /// which is [`Searched`] and nothing the user can see.
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
    /// RetroArch, under Steam at the head of the Games column: the way in to
    /// somebody's own ROM folder, and afterwards the way back out of it.
    ///
    /// The same row as [`Entry::Steam`] and for the same reasons — it is a way
    /// in to a column rather than a program to start, and where RetroArch's own
    /// desktop entry exists this one takes its place; see
    /// [`hide_retroarch_client`]. What differs is that this row is not always
    /// there at all: it exists on a machine that has the `lxb-retroarch`
    /// package and on no other. See [`crate::retroarch`].
    RetroArch(Emulation),
    /// One game in somebody's own ROM folder.
    ///
    /// Its own kind of row for the reasons [`Entry::Game`] is one, arrived at
    /// from the other end: nothing installed it, no `.desktop` file describes
    /// it, and what starts it is a core and a path rather than a command
    /// somebody wrote down. It is a *file* the shell knows how to play, which
    /// is what makes it neither an application nor one of the files under
    /// [`Entry::File`] — those are opened in whatever the desktop answers with,
    /// and this one is opened in the emulator the folder's name asked for.
    Rom(Rom),
    /// The row at the head of a column of folders that says "this one".
    ///
    /// The picker's answer, and the only row in that column that acts. A row
    /// rather than a control drawn over the column, on the terms
    /// [`Entry::Search`] is one — on this bar a row is the only thing there is
    /// — and it stands *over* the list for the reason the search field does:
    /// it is about the column rather than one of the things in it, so the
    /// column opens on the row below it and a press of A out of habit does not
    /// answer a question nobody has read yet.
    Pick(Pick),
    /// The row at the head of a folder's own listing that makes a new folder
    /// in it.
    ///
    /// Its own kind of row rather than a [`Pick`] wearing another word, and
    /// for the reason those two are not one: a `Pick` *answers* a question
    /// somebody else asked and takes the bar back out of the picker, and this
    /// one asks a question of its own — what shall the folder be called — and
    /// leaves the user standing exactly where they were.
    ///
    /// It stands over the list on the terms [`Entry::Pick`] and
    /// [`Entry::Search`] do: it is about the column rather than one of the
    /// things in it. And it is the reason an empty folder can be stepped into
    /// at all — a column with nothing in it is the one shape this bar cannot
    /// show, so before this row there was no way to make the first thing in an
    /// empty directory. See [`crate::files::listing`].
    Make(Make),
    /// The row at the head of the Trash column that empties it.
    ///
    /// Its own kind of row beside [`Entry::Make`] and on the same argument:
    /// they are both head rows that act, and what they do could not be less
    /// alike. One of them creates something and the other destroys everything
    /// in the column under it, and a single kind of row carrying which would
    /// be one mis-routed press away from the worst outcome in this shell.
    Sweep(Sweep),
    /// The row at the head of a column being marked: how many are ticked, and
    /// the way back out of the marking.
    ///
    /// A third acting head row beside [`Entry::Make`] and [`Entry::Sweep`], and
    /// it is never on a column with either of those: while it is there it
    /// stands in their place. See [`Done`].
    Done(Done),
    /// One thing somebody deleted, standing in the Trash column.
    ///
    /// A fourth kind of row about a file on this disk, and a fourth kind for
    /// the reason the third is: what can be done to it is not what can be done
    /// to a file. It cannot be opened — the program that would open it would be
    /// handed a path inside `~/.local/share/Trash/files` and a name the trash
    /// invented — it cannot be renamed, and it cannot be deleted, because it
    /// already has been. What it can be is put back or destroyed, and those are
    /// two rows no other kind of row in this shell carries.
    Trashed(crate::trash::Trashed),
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

/// The RetroArch row, as the second row of the Games column.
///
/// It carries what it says and nothing else, unlike [`Service`], which carries
/// the account. What pressing it does is decided by asking
/// [`crate::retroarch::RetroArch`] on the press — install, choose a folder, or
/// step across — because every one of those answers is a fact about a helper
/// process and a disk rather than about a row, and a row that carried a copy of
/// it would be a second opinion going stale between rebuilds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emulation {
    /// The line under the name: what is there, or what pressing it would do.
    comment: String,
}

impl Emulation {
    pub fn new(comment: String) -> Emulation {
        Emulation { comment }
    }
}

/// One game out of somebody's ROM folder.
///
/// Compared but not equatable: [`Rom::shape`] is a measurement of a picture,
/// and two measurements are alike or not alike rather than equal.
#[derive(Debug, Clone, PartialEq)]
pub struct Rom {
    /// The file's name without its extension, which is what somebody called
    /// the game when they saved it.
    pub name: String,
    pub path: PathBuf,
    /// The console it came out of, under the name this shell calls that console
    /// — "PlayStation Portable" rather than `psp`. Carried plainly as well as
    /// inside [`Rom::note`] because a panel raised over this row has to say it
    /// in a sentence of its own.
    pub console: String,
    /// The line under the name: which console, or why it cannot be started.
    pub note: String,
    /// The whole command line that starts it — RetroArch, the core, and this
    /// file — or `None` for a console with no core installed.
    ///
    /// Carried on the row rather than worked out on the press, and that is
    /// what lets a game go through [`crate::model::Lattice::launch_selected`]
    /// like every other row on this bar: the launcher has the catalogue and
    /// nothing else, and asking a helper process what to run at the moment
    /// somebody presses A would be a fork between the button going down and the
    /// loading screen. The row is rebuilt whenever the answer could have
    /// changed, which is whenever the folder is read again.
    ///
    /// The row exists either way: the game is still theirs, and a press says
    /// what is missing rather than nothing happening.
    pub start: Option<Vec<String>>,
    /// The cores that would run this console, best first — whether or not any
    /// of them is installed.
    ///
    /// What the press on a game with no [`Rom::start`] offers to fetch, and
    /// carried here for the reason `start` is: the press is holding a row, and
    /// the answer has to be on it. Which of these actually exists for this
    /// machine is not decided here; the whole list goes to the helper, which
    /// takes the first that libretro publishes. See
    /// [`crate::retroarch::RetroArch::fetch`].
    pub wanted: Vec<String>,
    /// The game's cover on this disk, where there is one — what the row *is*,
    /// rather than the mark every row of the column would otherwise wear.
    ///
    /// A path to a file in the shell's own cache rather than a picture, and it
    /// goes through the same worker and the same band of the same atlas as a
    /// thumbnail of any other file: a cover is a picture on a card, and so is a
    /// photograph. See [`crate::retroarch`], which is where it comes from, and
    /// [`crate::thumbs`], which decodes it.
    ///
    /// `None` for a game nothing has a picture of, which is a row that looks
    /// exactly as it did before any of this existed.
    pub boxart: Option<PathBuf>,
    /// A screenshot of it on this disk, on the same terms. It stands behind the
    /// whole display while the cursor is on the row, blurred — see
    /// [`crate::thumbs::Want::Snapshot`].
    pub snap: Option<PathBuf>,
    /// Whether [`Rom::boxart`] is a picture somebody chose themselves rather
    /// than one libretro published — see `crate::retroarch::Shown`.
    pub own_cover: bool,
    /// The same for [`Rom::snap`], and this one changes how it is *drawn*.
    ///
    /// What libretro holds is a photograph of a console's screen, three hundred
    /// pixels tall, which is blurred on its way across a television because
    /// enlarged honestly it is a wall of squares. A picture somebody chose is
    /// theirs, at whatever size they chose it, and blurring it would be the
    /// shell smearing a photograph nobody asked it to touch. See
    /// `Shell::sight_of`, which is where the two part.
    pub own_background: bool,
    /// The shape this console's covers are: the width of one over its height.
    ///
    /// Not one number for all of them, because a console's boxes are its own. A
    /// Nintendo DS case is wider than it is tall, a Wii case is taller than a
    /// PlayStation 2 one, and a UMD case is taller again. Drawn at a single
    /// shape, every shelf but the one that shape was taken from showed a cover
    /// that did not fill the card it stood on — a square DS box in a portrait
    /// card, with a band of glass above and below it that read as the picture
    /// having been put down carelessly rather than as a mount.
    ///
    /// Measured off the covers on this disk rather than looked up in a table of
    /// consoles, which is the rule the whole of this integration keeps: the
    /// picture already knows what shape it is, and a table would be this shell
    /// asserting the dimensions of somebody else's artwork. See
    /// [`crate::retroarch::shelf_shape`].
    ///
    /// A fact about the *console* rather than about this row, and carried on
    /// every row of the shelf because the shelf is laid out from the first of
    /// them: one card shape per column, or the rows would step in and out from
    /// one to the next. `None` where nothing has been measured — a shelf whose
    /// covers have not been fetched is a column of marks, and it keeps the
    /// shape a column of covers had before any of this.
    pub shape: Option<f32>,
    /// The mark to draw where there is no cover: this game's console.
    ///
    /// Never empty — the integration resolves it to RetroArch's own mark where
    /// the console has none, so a row always has something to wear. It is a
    /// `String` rather than a `&'static str` because it names a drawing that
    /// arrived with a *package*, and there is no static list of those.
    pub glyph: String,
}

/// The row that answers a folder picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The folder it is in, which is what pressing it answers with. Carried
    /// rather than read back off the column, because by the time the press is
    /// answered the panel raised over it is what is on screen.
    pub at: PathBuf,
    /// What the answer is for.
    pub about: crate::settings::Picking,
    /// The line under it: what choosing this folder would mean.
    pub comment: String,
}

/// The row that makes a new folder in the one the column is of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Make {
    /// The folder the new one goes in, which is the folder this column is a
    /// listing of. Carried rather than read back off the column, for the
    /// reason [`Pick`] carries its own: by the time a name has been typed the
    /// listing has been rebuilt under the caret more than once.
    pub at: PathBuf,
}

/// The row that empties the trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// How many things are in there, which is the line under the row: a press
    /// that would destroy four hundred files should say four hundred before it
    /// is made and not after.
    pub items: usize,
    /// That count, written out. Held rather than formatted where the row is
    /// drawn, because [`Entry::comment`] hands back a borrow of what the row
    /// is carrying.
    note: String,
}

impl Sweep {
    pub fn new(items: usize) -> Self {
        let note = match items {
            1 => "1 item".to_string(),
            items => format!("{items} items"),
        };
        Self { items, note }
    }

    fn note(&self) -> &str {
        &self.note
    }
}

/// The row at the head of a column that is being marked, which says how many
/// rows are ticked and is the way back out.
///
/// It stands where the column's own acting head row stands, and while it is
/// there that row is gone: a folder being marked has no New folder and the
/// trash being marked has no Empty trash. One acting row at the top, and while
/// the marking is on it is the marking's — anything else would be a column
/// offering to make a folder in the middle of somebody counting up what they
/// are about to move out of it.
///
/// The field stays. Narrowing a column while marking it is a reasonable thing
/// to want, the marks are held by path and survive the read, and the search is
/// not a thing that *acts*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// How many are ticked, which is the line under the row.
    pub picked: usize,
    /// That count, written out — held rather than formatted where the row is
    /// drawn, for the reason [`Sweep`] holds its own.
    note: String,
}

impl Done {
    pub fn new(picked: usize, note: String) -> Self {
        Self { picked, note }
    }

    fn note(&self) -> &str {
        &self.note
    }
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
/// Three answers and they are answered in three different places, which is the
/// whole reason this is a type. A shelf is half a million files held on a
/// worker, so narrowing one is a message to that worker and the rows come back
/// when they are ready. A folder is one directory on the disk, so narrowing one
/// is reading it again — which costs a `readdir` and a `stat` per row kept, and
/// is done on the frame the letter was typed. A Steam library is a few hundred
/// titles the shell is already holding, so narrowing one is a `contains` per
/// game and is likewise done on that frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Searched {
    Shelf(crate::media::Kind),
    Folder,
    /// Somebody's Steam library, which is a column of games the shell owns the
    /// whole of. See [`crate::steam::Steam::rows`].
    Library,
}

impl Searched {
    /// The shelf this is, if it is one. `None` for a folder, which is what
    /// keeps everything the media library does away from the explorer's rows.
    pub fn shelf(self) -> Option<crate::media::Kind> {
        match self {
            Searched::Shelf(kind) => Some(kind),
            Searched::Folder | Searched::Library => None,
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
            // What the account owns, which is what the count is of: a library
            // of six hundred games says "4 of 600 games match" whether or not
            // any of them is on this machine's disk.
            Searched::Library => "games",
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
                    Searched::Library => "Search this library by name".to_string(),
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
    /// Whether this row stands *over* the column rather than being one of the
    /// rows it is a list of.
    ///
    /// One of these too — the index at the head of a Steam library, which is
    /// the whole of that library again by the letter each game starts with. It
    /// is not one of the games, so a column that opened on it would open on a
    /// control nobody asked for, and every visit to somebody's library would
    /// begin by stepping down off it.
    ///
    /// That is the same thing the field at the head of a shelf needs and gets
    /// — see [`head_rows`], which is where the two are counted together, and
    /// [`Entry::over_the_list`], which is the question asked of a row.
    pub over_the_list: bool,
    /// Which account this row opens the form for, where it opens one.
    ///
    /// `None` for every subcategory in the shell but the ones under Settings >
    /// Users. It is on the row for the reason [`Folder::place`] is: what is
    /// behind this column is not a list the tree wrote but a *draft* held
    /// elsewhere, and the shell has to be able to tell, from the trail the
    /// cursor has walked, that the column it is standing in is that form — see
    /// [`crate::users`], and `Shell::sync_user_form`, which is where the draft
    /// is opened and thrown away.
    pub person: Option<crate::users::Whose>,
    /// The picture this row wears *instead of* its glyph, cut round.
    ///
    /// One page uses it, and it is the whole of what that page looks like: an
    /// account is a person, and the thing that says which person is their own
    /// picture rather than any mark the shell could draw. Where there is none
    /// the row falls back to its icon, which is the single figure — and
    /// deliberately not the two figures the page itself is reached by, or every
    /// row on it would wear the heading above it.
    ///
    /// A path rather than a picture, and it goes through the same worker and
    /// the same band of the same atlas as the thumbnail of any other file. See
    /// [`crate::thumbs`].
    pub portrait: Option<PathBuf>,
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
    /// The material this row stands for, when the setting is a material.
    ///
    /// The same argument as [`Self::swatch`], made about the other thing a row
    /// can stand for that no word describes. A colour's row is drawn *in* that
    /// colour; a material's row is drawn *in* that material — the mark on it is
    /// shaded as a bead of water or laid down flat according to what pressing
    /// the row would do, whatever the shell is set to at the time. It is what
    /// lets the two rows of a Theme column carry the same drawing, which they
    /// do: the difference between the materials is not in the shape.
    ///
    /// `None` everywhere else, which is every row in the tree but four. See
    /// [`crate::gpu::Quad::mark`], which is where it ends up.
    pub material: Option<Style>,
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
    /// Whether this row stands *over* the column rather than being one of the
    /// rows it is a list of — the same question [`Folder::over_the_list`]
    /// answers, asked of a row that acts.
    ///
    /// One row in the shell: *Use no avatar*, at the head of the walk that
    /// chooses somebody's picture. It belongs there because it is the other
    /// answer to what that whole column asks — not a file, but a way of having
    /// no file — and it must not be what the column opens on, for the reason
    /// [`Entry::Pick`] gives: a press of A out of habit would answer a question
    /// nobody has read yet.
    pub over_the_list: bool,
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

/// A top-level lattice column.
#[derive(Debug, Clone)]
pub struct Category {
    pub id: &'static str,
    pub title: &'static str,
    /// Icon name looked up in the icon theme.
    pub icon: &'static str,
    pub entries: Vec<Entry>,
}

/// A column with its rows left behind: what it is called, and what it is drawn
/// with.
///
/// The Settings tree needs the bar to build one row — Startup category, which
/// is a list of the columns this machine has — and it cannot be handed the bar
/// itself: the tree is built *into* the bar, so a page holding a lattice would
/// be a lattice holding itself. This is the part of a column that page needs,
/// and all three fields are already `&'static str`, so a list of these borrows
/// nothing and can be taken before the column it came from is reached for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
}

/// Every column the shell can put on a bar, in bar order.
///
/// Not what *this* machine has — that is the bar, and it is what the page is
/// built from. This is the wider set, asked one question only: a setting naming
/// a column that is not on the bar this session has to be able to say what it
/// named, or somebody who set the shell to open on Steam and then signed out
/// would find the page showing nothing chosen. See [`known_column`].
pub fn every_column() -> Vec<Column> {
    let mut ranked: Vec<Column> = CATEGORY_TABLE
        .iter()
        .map(|(id, title, icon, _)| Column { id, title, icon })
        .chain([STEAM, retroarch_column_id()].map(|(id, title, icon)| Column { id, title, icon }))
        .collect();
    ranked.sort_by_key(|column| rank(column.id).unwrap_or(usize::MAX));

    // The shell's own column is not ranked and is always first — which is the
    // whole of what it needs from [`rank`], there and here. See
    // [`column_place`].
    let (id, title, icon) = SHELL_SETTINGS;
    let mut columns = vec![Column { id, title, icon }];
    columns.extend(ranked);
    columns
}

/// What a column of this name is called and drawn with, whether or not this
/// machine has one.
pub fn known_column(id: &str) -> Option<Column> {
    every_column().into_iter().find(|column| column.id == id)
}

/// The shell's own column, for settings that belong to LineXinBar itself rather
/// than to anything installed on the system.
///
/// Always present and always first, the way a console shell opens on Settings.
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
/// Software and Waydroid are named here for a fourth reason: an entry is filed
/// into either of them by something other than an XDG main category, so
/// [`App::category_id`] has to be able to say their names before the table is
/// consulted at all. See [`is_store`] and [`is_android`].
const SOFTWARE: &str = "software";
const WAYDROID: &str = "waydroid";

/// Where installed applications go, in lattice order.
///
/// Each entry lists the XDG main categories that map onto it, and the first
/// match wins. `Settings` and `System` share a column, as they do in Plasma —
/// its menu has no Settings menu of its own, and the shell's own Settings
/// column is not somewhere an installed application belongs.
///
/// Two columns name no categories at all. Software and Waydroid are not
/// questions a main category answers — a store says `System` like every other
/// administrative tool, and an Android application says nothing a menu has ever
/// heard of — so both are claimed before this table is consulted and their
/// lists are deliberately empty. See [`App::category_id`]. They are in the
/// table all the same, because this is also the order the bar is in and both of
/// them are places for something rather than consequences of something.
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
    // Where the machine gets more of itself from. Its list is empty because
    // nothing is filed here by a main category: a store is recognised by
    // [`is_store`] and claimed before this table is read.
    (SOFTWARE, "Software", crate::icons::CATEGORY_SOFTWARE, &[]),
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
    // The Android applications this session can run, for the reason above:
    // Waydroid writes `X-WayDroid-App` and no main category at all, so its
    // entries are claimed by [`is_android`] rather than found here.
    (WAYDROID, "Waydroid", crate::icons::CATEGORY_WAYDROID, &[]),
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
                over_the_list: false,
                person: None,
                portrait: None,
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
        place: Some(crate::files::Place::Volumes(
            crate::files::Shows::Everything,
        )),
        chosen: false,
        over_the_list: false,
        person: None,
        portrait: None,
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
pub fn place_rows(
    listing: Vec<Entry>,
    query: &str,
    found: usize,
    pick: Option<Pick>,
    make: Option<Make>,
) -> Vec<Entry> {
    let mut rows = Vec::with_capacity(listing.len() + 3);
    // Above the field, where the picker's Paste row sits, and for the reason
    // Paste sits there: it is the row that is not about the list at all. The
    // field keeps the place it has always had — directly over the files — so
    // Up from the first row of a folder still lands on the search it has
    // always landed on, and the new row is one further up for somebody who
    // went looking for it.
    //
    // It is also the only row an empty folder has. A column with nothing in it
    // cannot be stepped into, so before this the first thing in an empty
    // directory was one somebody had to make from a terminal.
    if let Some(make) = make {
        rows.push(Entry::Make(make));
    }
    match pick {
        // A column being walked to *choose* it carries the answer at its head
        // and no field: the rows are not what the user came for, the folder is
        // — the same argument the folder picker a file is carried to makes, and
        // the same shape. See [`crate::transfer`].
        Some(pick) => rows.push(Entry::Pick(pick)),
        None => head(&mut rows, Searched::Folder, query, listing.len(), found),
    }
    rows.extend(listing);
    rows
}

/// The rows that stand over a list: the field, and the row that empties it.
///
/// The one place either is made, so a field is the same object wherever it is
/// on this bar — the shelves and the explorer build their columns here, and the
/// Steam library builds its own beside its index. See [`Searched`] for what
/// differs between them, which is only where the narrowing happens.
///
/// `found` is how many there are altogether and `matched` how many the query
/// has kept, so a list narrowed to nothing still carries the two rows that say
/// why. Nothing at all to search gets neither row: a column holding only the
/// offer to search it is a column worth stepping into for nothing.
pub fn head(rows: &mut Vec<Entry>, of: Searched, query: &str, matched: usize, found: usize) {
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

/// How many rows at the head of a column stand over it rather than being what
/// the column is a list *of*.
///
/// A column of music opens on music, and a library of games opens on a game.
/// What stands over a list stands in the place anything standing over a list
/// stands, and is reached by pressing Up from the top of it — which is the one
/// direction nothing else was using, and where a person looks for the thing
/// above the first thing. Opening *on* one would make every visit to a shelf
/// start by stepping over a control the user did not ask for.
///
/// Two kinds of them in the shell, and they are one idea rather than two: the
/// field that searches what the column is a list of, and the index at the head
/// of a Steam library. A library carries both, in that order — the letters are
/// the way into the list under them, and the field is the way into everything
/// including the letters. See [`Entry::over_the_list`].
pub fn head_rows(entries: &[Entry]) -> usize {
    entries
        .iter()
        .take_while(|entry| entry.over_the_list())
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
        dropped |= forget_below(&mut category.entries, &|at| at == path);
    }
    dropped
}

/// The same for a whole folder that has gone: every row for anything that was
/// inside it, wherever on the bar it is.
///
/// Deleting a folder is the one act in this shell that takes rows off columns
/// the user was not looking at. `~/Music/Live at Leeds` is twelve songs on the
/// Music shelf, and a shelf still offering to play them after the folder has
/// gone to the trash is twelve rows that would each start a player pointed at
/// nothing.
///
/// `starts_with`, which on a `Path` compares whole components rather than
/// characters: `~/Music/Live` is not a prefix of `~/Music/Liverpool`, and a
/// rule written on the strings would have taken that folder as well.
pub fn forget_folder(categories: &mut [Category], folder: &std::path::Path) -> bool {
    let mut dropped = false;
    for category in categories {
        dropped |= forget_below(&mut category.entries, &|at| at.starts_with(folder));
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
fn forget_below(entries: &mut Vec<Entry>, gone: &dyn Fn(&std::path::Path) -> bool) -> bool {
    let before = entries.len();
    entries.retain(|row| match row {
        Entry::Media(file) => !gone(&file.path),
        Entry::File(file) => !gone(&file.path),
        // A game in a console's column is a file on somebody's disk like the
        // other two, and a bar still offering to play one that has just been
        // deleted is a bar that has not understood. The column is rebuilt from
        // the helper's answer a moment later; this is what takes the row off it
        // in the meantime.
        Entry::Rom(rom) => !gone(&rom.path),
        _ => true,
    });
    let mut dropped = entries.len() != before;
    for entry in entries {
        if let Entry::Folder(folder) = entry {
            dropped |= forget_below(&mut folder.entries, gone);
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

/// The column somebody's own ROM folder hangs in.
///
/// Not in [`CATEGORY_TABLE`] either, and for one reason more than Steam's: it
/// is a consequence of a *package* being installed as well as of a folder
/// having been chosen, so on most machines this column can never exist at all.
/// Its mark arrives with that package — see [`crate::retroarch::mark`], which
/// is the same mark its rows wear.
fn retroarch_column_id() -> (&'static str, &'static str, &'static str) {
    ("retroarch", "RetroArch", crate::retroarch::mark())
}

/// What that column is called on the bar, for the two things outside this
/// module that have to find it: pressing the RetroArch row takes the display
/// to it, and so does finishing the setup it asks for.
pub fn retroarch_column() -> &'static str {
    retroarch_column_id().0
}

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

/// Take RetroArch's own `.desktop` entry off the bar.
///
/// The same act as [`hide_steam_client`] and for the same reason: two rows
/// called RetroArch wearing one mark is the bar saying the same thing twice,
/// and of the two it is the shell's own that leads somewhere — a column of the
/// user's games, which is what somebody pressing a row called RetroArch on a
/// games console is after.
///
/// Called only on a session that has a RetroArch row of its own to offer,
/// which is one whose machine has the `lxb-retroarch` package. Without it the
/// entry stays where the scan filed it and is the only way to RetroArch, which
/// is exactly right.
///
/// `true` when there was one to take.
pub fn hide_retroarch_client(categories: &mut Vec<Category>) -> bool {
    let mut taken = false;
    let mut emptied = Vec::new();
    for (at, column) in categories.iter_mut().enumerate() {
        let before = column.entries.len();
        column.entries.retain(|entry| {
            // A machine with both the distribution package and the flatpak has
            // two entries, and both go: which of them would be *used* is the
            // helper's answer rather than this one — see `lxb-retroarch`'s
            // `find`, which prefers the native package.
            let is_retroarch = entry.app().is_some_and(|app| app.owns_window("retroarch"));
            taken |= is_retroarch;
            !is_retroarch
        });
        if column.entries.len() != before && !column.has_launchable() {
            emptied.push(at);
        }
    }
    for at in emptied.into_iter().rev() {
        categories.remove(at);
    }
    taken
}

/// Put the RetroArch row under the Steam row in the Games column, or take it
/// away.
///
/// `comment` is the line under the name, which is the whole of what the row
/// carries — see [`Emulation`]. `None` takes the row off, which is what a
/// session whose helper has gone does.
///
/// Under Steam and not above it: Steam is the row every session has and this
/// one is a package, and a row that arrived with an install must not push the
/// one that was always there down a place. The row is rebuilt rather than
/// edited, exactly as the Steam row above it is.
pub fn offer_retroarch(categories: &mut Vec<Category>, comment: Option<String>) -> Shifted {
    let mut shifted = Shifted::default();

    let standing = categories.iter().position(|column| column.id == GAMES);
    let at = match (standing, &comment) {
        (Some(at), _) => at,
        // Nothing to say and no column to say it in: the ordinary state of a
        // machine without the package.
        (None, None) => return shifted,
        // The offer is enough to earn a column, on the terms the Steam row
        // earns one: from that row a whole other column is one press away.
        (None, Some(_)) => {
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
    column
        .entries
        .retain(|entry| !matches!(entry, Entry::RetroArch(_)));
    if let Some(comment) = comment {
        // Under the Steam row where there is one, and at the head where there
        // is not — a session started with `--no-steam` has no Steam row, and
        // this row would then be standing under nothing.
        let under = usize::from(
            column
                .entries
                .first()
                .is_some_and(|first| matches!(first, Entry::Steam(_))),
        );
        column
            .entries
            .insert(under, Entry::RetroArch(Emulation::new(comment)));
    }
    shifted
}

/// Hang somebody's ROM folder in a column of its own, or take the column away.
///
/// The same shape as [`shelve_steam`] and for the same reasons; the rows
/// arrive already in the order they go in, because that order belongs to the
/// library rather than to the bar — see [`crate::retroarch::RetroArch::rows`].
pub fn shelve_retroarch(categories: &mut Vec<Category>, rows: Vec<Entry>) -> Shifted {
    let mut shifted = Shifted::default();
    let (id, title, icon) = retroarch_column_id();
    let standing = categories.iter().position(|column| column.id == id);

    match (standing, rows.is_empty()) {
        (None, true) => {}
        (Some(at), true) => {
            categories.remove(at);
            shifted.removed = Some(at);
        }
        (Some(at), false) => categories[at].entries = rows,
        (None, false) => {
            let at = column_place(categories, id);
            categories.insert(
                at,
                Category {
                    id,
                    title,
                    icon,
                    entries: rows,
                },
            );
            shifted.added = Some(at);
        }
    }
    shifted
}

/// Whether this row opens a column of a folder chooser.
///
/// Which is what the head of the chooser is, and what every folder in it is:
/// the walk is folders all the way down, and each of them is listed by the same
/// rules — folders only, with the row that answers the question standing over
/// them.
///
/// What it is for is knowing how far to come back out when the question has
/// been answered. A column is one of the chooser's when the row it was *opened
/// from* is one of these, which is the question this answers and the reason it
/// is asked of a row rather than of a column's contents: the page under
/// Settings > Games > RetroArch has this row in it and is not part of any walk,
/// so a shell that asked "does this column contain one" would come back out of
/// the settings page the row belongs to as well. See `Shell::leave_the_picker`.
pub fn opens_a_picker(entry: &Entry) -> bool {
    match entry {
        Entry::Folder(folder) => folder
            .place
            .as_ref()
            .is_some_and(|place| place.shows().picking().is_some()),
        _ => false,
    }
}

/// Where the row that opens this picker stands, if the column has one.
///
/// Looked for rather than counted to, because these rows come and go: the head
/// of the RetroArch column is the games chooser until somebody has chosen a
/// folder with games in it, and then it is the first console. A shell that
/// opened "the row at the top" would open a console the day the setup was
/// finished.
///
/// And by *which* picker, because a column may hold two: the RetroArch column
/// carries the games folder and, on a machine missing one, a console's BIOS.
/// They ask different questions and a press meant for one must not open the
/// other.
pub fn picker_row_for(entries: &[Entry], about: crate::settings::Picking) -> Option<usize> {
    entries.iter().position(|entry| match entry {
        Entry::Folder(folder) => {
            folder
                .place
                .as_ref()
                .and_then(|place| place.shows().picking())
                == Some(about)
        }
        _ => false,
    })
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

/// How far along the bar a column belongs, in quarter-steps.
///
/// The table's own order, times four, so that a column which is not in the
/// table can sit *between* two that are without either of them having to move.
/// There are two such columns and they are both libraries of games, so they
/// both belong immediately after Games — a person who has just been looking at
/// what is installed and steps right lands in what they own. Steam is first of
/// the two because it is the one every session has; RetroArch is a package a
/// machine may not have at all, and a column that comes and goes with a
/// package must not move the one that does not.
///
/// Four rather than three because the room is worth having: the next column
/// that is a consequence of something rather than a place for something has
/// somewhere to go without this being re-solved.
fn rank(id: &str) -> Option<usize> {
    if id == STEAM.0 {
        return rank(GAMES).map(|games| games + 1);
    }
    if id == retroarch_column_id().0 {
        return rank(GAMES).map(|games| games + 2);
    }
    CATEGORY_TABLE
        .iter()
        .position(|(own, ..)| *own == id)
        .map(|place| place * 4)
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

        // The same list, spelled the same way. `Keywords` is localised like
        // `Name` is, and the localised copy is the one a user searching in
        // their own language would find the entry by — so it is the one read,
        // and the two questions asked of it below are both asked
        // case-insensitively for that reason.
        let keywords = localised(&fields, "Keywords")
            .map(|list| {
                list.split(';')
                    .map(str::trim)
                    .filter(|word| !word.is_empty())
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
            keywords,
            mime_types,
            path: path.to_path_buf(),
            wm_class: fields.get("StartupWMClass").cloned(),
        })
    }

    /// Which column this app belongs in.
    ///
    /// Two questions come before the table, and both are asked first because
    /// they are *more specific* claims than anything a main category makes. An
    /// Android application says `X-WayDroid-App` and nothing else a menu
    /// understands, so left to the table it would fall through to Other — which
    /// is where they were. And a store says `System` like every other
    /// administrative tool on the machine, so left to the table it would be
    /// filed with the disk utilities, which is not what somebody looking for
    /// somewhere to get software from is after.
    fn category_id(&self) -> &'static str {
        if self.is_android() {
            return WAYDROID;
        }
        if self.is_store() {
            return SOFTWARE;
        }
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

    /// Whether this entry is one Waydroid wrote for an Android application.
    ///
    /// One category and nothing else. `X-WayDroid-App` is what Waydroid puts on
    /// every entry it generates and on its own launcher, it is in the `X-`
    /// namespace so nothing else on the machine can be using it by accident,
    /// and it is the only thing those entries have in common — a calculator, a
    /// browser and a game share no subject, only where they come from.
    ///
    /// Waydroid's own launcher is one of them, and belongs here: a row that
    /// starts the whole Android session standing at the head of the column of
    /// what that session runs is the same arrangement Steam has in Games.
    pub fn is_android(&self) -> bool {
        self.categories
            .iter()
            .any(|own| own.eq_ignore_ascii_case(ANDROID_CATEGORY))
    }

    /// Whether this entry is a store or a software hub.
    ///
    /// Two ways to be one, because the registered answer does not cover the
    /// stores people actually have.
    ///
    /// The first is `PackageManager`, which is the XDG additional category for
    /// exactly this and is what a well-described store says. The spec requires
    /// it to be paired with `System` or `Settings`, and that pairing is
    /// enforced here rather than ignored — it is what keeps a game's mod
    /// manager, which declares `Game;PackageManager`, in Games where somebody
    /// looking for it would go.
    ///
    /// The second is a short list of the stores that do not declare it. Plasma
    /// Discover says `Qt;KDE;System` and nothing more, and a column that could
    /// not hold Discover would be a Software column on a Plasma machine with
    /// nothing in it. The list is matched against the entry's own file name,
    /// which is the one part of a `.desktop` file that is an identity rather
    /// than a description — see [`KNOWN_STORES`].
    pub fn is_store(&self) -> bool {
        let says = |wanted: &str| {
            self.categories
                .iter()
                .any(|own| own.eq_ignore_ascii_case(wanted))
        };
        if says("PackageManager") && (says("System") || says("Settings")) {
            return true;
        }
        let Some(stem) = self.path.file_stem().and_then(|stem| stem.to_str()) else {
            return false;
        };
        KNOWN_STORES
            .iter()
            .any(|known| stem.eq_ignore_ascii_case(known))
    }

    /// Whether this application has asked for its icon to be drawn in the
    /// shell's own material rather than as the picture the theme holds.
    ///
    /// The word is `lxb`, in `Keywords` or in `Categories`, and it is the one
    /// thing in a `.desktop` file an application can say *to this shell*. What
    /// it buys is what every mark the shell draws itself gets: the drawing is
    /// measured into a distance field and the quad shader cuts a bead of water
    /// to it, so the row wears the same material as the column heading above
    /// it. See [`crate::icons::shaped`].
    ///
    /// Opt-in, and it has to be. What the shader is handed is the *silhouette*
    /// of the icon and nothing else — a photograph or a full-bleed square logo
    /// comes out as a rounded slab of glass, which is the right answer only for
    /// a drawing that was made to be one. An application that says the word has
    /// said its icon is a shape.
    ///
    /// `Categories` as well as `Keywords` because an entry is entitled to say
    /// it either way and neither spelling is more true than the other. `lxb` is
    /// not a registered main category, so a file naming it there loses nothing:
    /// [`Self::category_id`] never matches on it.
    pub fn wears_shell_material(&self) -> bool {
        let mark = |word: &String| word.eq_ignore_ascii_case(SHELL_MATERIAL_KEYWORD);
        self.keywords.iter().any(mark) || self.categories.iter().any(mark)
    }
}

/// What Waydroid writes on every entry it generates. See [`App::is_android`].
const ANDROID_CATEGORY: &str = "X-WayDroid-App";

/// The word an application puts in its `Keywords` or `Categories` to ask for
/// the shell's own material. See [`App::wears_shell_material`].
const SHELL_MATERIAL_KEYWORD: &str = "lxb";

/// The stores that do not say `PackageManager`, by the name of their entry.
///
/// A list rather than a rule, and it is worth being honest about what that
/// costs: a store this does not know and that does not declare itself is filed
/// wherever its categories put it, which on nearly every one of them is System.
/// That is the state the whole machine was in before this column existed, so
/// the list can only improve on it — but it is a list, and a machine's package
/// manager is exactly the sort of thing there is one more of every year.
///
/// By file name because that is the one field of a `.desktop` file that
/// identifies rather than describes: `Name` is localised and changes with the
/// user's language, `Exec` is a path that differs between a distribution
/// package and a flatpak, and the file name is the id every other desktop keys
/// its own overrides off.
const KNOWN_STORES: &[&str] = &[
    // Plasma's, under both the name it has now and the one it shipped under.
    "org.kde.discover",
    "plasma-discover",
    // GNOME's, and Ubuntu's and Mint's re-skins of the same idea.
    "org.gnome.Software",
    "gnome-software",
    "snap-store",
    "io.snapcraft.SnapStore",
    "ubuntu-software",
    "mintinstall",
    // elementary's.
    "io.elementary.appcenter",
    // The Flatpak managers that are hubs rather than permission editors.
    "io.github.flattool.Warehouse",
    // Arch's two, and Manjaro's spelling of the first.
    "octopi",
    "pamac-manager",
    "org.manjaro.pamac.manager",
    // Fedora's, and the desktop front end PackageKit ships.
    "dnfdragora",
    "yumex",
    "gpk-application",
    // Debian's oldest, still installed on a great many machines.
    "synaptic",
    // The one that takes them all at once.
    "bauh",
];

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
            Entry::RetroArch(_) => "RetroArch",
            Entry::Game(game) => &game.name,
            Entry::Rom(rom) => &rom.name,
            Entry::Pick(_) => "Select folder",
            Entry::Make(_) => "New folder",
            Entry::Sweep(_) => "Empty trash",
            Entry::Done(_) => "Done",
            // What it was called before it was deleted, which is not what it
            // is filed as: two files of one name are `holiday.mp4` and
            // `holiday.mp4.2` in `files/`, and a column showing the second one
            // that would be showing a name the trash invented.
            Entry::Trashed(item) => &item.name,
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
            Entry::RetroArch(emulation) => Some(&emulation.comment),
            Entry::Game(game) => Some(&game.note),
            Entry::Rom(rom) => Some(&rom.note),
            Entry::Pick(pick) => Some(&pick.comment),
            // Where it will go, said plainly, because the row is a press away
            // from a keyboard and somebody standing on it has not read a menu.
            Entry::Make(_) => Some("Make a folder in this one"),
            Entry::Sweep(sweep) => Some(sweep.note()),
            Entry::Done(done) => Some(done.note()),
            // Where it came from and when it went — see
            // [`crate::trash::Trashed`]. Empty for an entry whose ticket the
            // shell could not read, which is a row that says nothing rather
            // than a row that guesses.
            Entry::Trashed(item) => Some(item.note.as_str()).filter(|note| !note.is_empty()),
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
            // The mark that arrived with the package, which is the one thing
            // every row of that column has in common. A machine whose package
            // is there but whose drawing is not falls back to the pad — see
            // [`crate::retroarch::mark`].
            Entry::RetroArch(_) => Some(crate::retroarch::mark()),
            // Its console's mark, not the emulator's: a shelf of PlayStation
            // games whose covers have not arrived should still look like
            // PlayStation games. See [`Rom::glyph`].
            Entry::Rom(rom) => Some(&rom.glyph),
            // The folder it would answer with, drawn as a folder: what is
            // being chosen is the column the row stands over.
            // The tick and not a folder: every other row in the column it
            // stands over is a folder, and the one row that is an *answer* has
            // to look like one. See [`crate::icons::CHOSEN`], which is the mark
            // every value in force on this bar wears.
            Entry::Pick(_) => Some(crate::icons::CHOSEN),
            Entry::Make(_) => Some(crate::icons::NEW_FOLDER),
            Entry::Sweep(_) => Some(crate::icons::TRASH_EMPTY),
            Entry::Done(_) => Some(crate::icons::SELECT_MULTIPLE),
            // Whatever the file's own name says it is, which is the table the
            // explorer draws a listing with: a song looks like a song in the
            // trash as well, and a folder looks like a folder.
            Entry::Trashed(item) => Some(item.glyph()),
            Entry::Facts(facts) => Some(&facts.icon),
            Entry::Typed(typed) => Some(&typed.icon),
        }
    }

    /// Whether this row stands over the column's list rather than being one of
    /// it.
    ///
    /// True of a shelf's two search rows, which are about the list rather than
    /// in it, and of the index at the head of a Steam library. Everything else
    /// on this bar is one of the things its column is a list of — including
    /// every other subcategory, which is why this is a question about the row
    /// and not about its kind. See [`head_rows`].
    pub fn over_the_list(&self) -> bool {
        match self {
            Entry::Search(_)
            | Entry::Pick(_)
            | Entry::Make(_)
            | Entry::Sweep(_)
            | Entry::Done(_) => true,
            Entry::Folder(folder) => folder.over_the_list,
            Entry::Choice(choice) => choice.over_the_list,
            _ => false,
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

    /// And the same column as the list it really is, for the one thing that
    /// needs to put a row on it or take one off rather than change one.
    ///
    /// A `Vec` where [`Entry::entries_mut`] hands back a slice, and the caller
    /// is the head row a marking stands at the top of a column — see
    /// [`crate::marks`]. Nothing else in this shell adds a row to a column that
    /// is already on screen: every other column is built whole and replaced
    /// whole.
    pub fn entries_vec_mut(&mut self) -> Option<&mut Vec<Entry>> {
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

    /// Whether this is the RetroArch row itself.
    ///
    /// It carries no state to hand back — see [`Emulation`] — so this answers
    /// with the row and the caller asks [`crate::retroarch::RetroArch`] what
    /// pressing it should do.
    pub fn emulation(&self) -> Option<&Emulation> {
        match self {
            Entry::RetroArch(emulation) => Some(emulation),
            _ => None,
        }
    }

    /// The game out of somebody's ROM folder this row is, if it is one.
    pub fn rom(&self) -> Option<&Rom> {
        match self {
            Entry::Rom(rom) => Some(rom),
            _ => None,
        }
    }

    /// The column this row opens, where the row carries it itself.
    ///
    /// What reads it is the press that opens a folder chooser: a chooser is a
    /// row with a [`crate::files::Place`] on it, and which question it is
    /// asking is written there rather than anywhere the walk can reach.
    pub fn folder(&self) -> Option<&Folder> {
        match self {
            Entry::Folder(folder) => Some(folder),
            _ => None,
        }
    }

    /// The folder this row would answer a picker with, if it is that row.
    pub fn pick(&self) -> Option<&Pick> {
        match self {
            Entry::Pick(pick) => Some(pick),
            _ => None,
        }
    }

    /// The folder a new one would be made in, if this is the row that makes
    /// one.
    pub fn make(&self) -> Option<&Make> {
        match self {
            Entry::Make(make) => Some(make),
            _ => None,
        }
    }

    /// Whether this is the row that empties the trash, and how much it would
    /// destroy.
    pub fn sweep(&self) -> Option<&Sweep> {
        match self {
            Entry::Sweep(sweep) => Some(sweep),
            _ => None,
        }
    }

    /// The row that ends a marking, if this is it.
    pub fn done(&self) -> Option<&Done> {
        match self {
            Entry::Done(done) => Some(done),
            _ => None,
        }
    }

    /// The trashed thing this row stands for, if it stands for one.
    pub fn trashed(&self) -> Option<&crate::trash::Trashed> {
        match self {
            Entry::Trashed(item) => Some(item),
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
            Entry::App(_) | Entry::Media(_) | Entry::File(_) | Entry::Game(_) | Entry::Rom(_)
        )
    }

    /// The picture this row wears instead of its glyph — see
    /// [`Folder::portrait`].
    ///
    /// Asked of every row rather than only of folders, because the place that
    /// draws it is asking one question about a whole column: is there a picture
    /// to put in the round hole where a mark would be. A file in the explorer
    /// answers it with its own thumbnail, and an account answers it with a
    /// photograph of the person.
    pub fn portrait(&self) -> Option<&std::path::Path> {
        match self {
            Entry::Folder(folder) => folder.portrait.as_deref(),
            _ => None,
        }
    }

    /// Which account's form this row opens — see [`Folder::person`].
    pub fn person(&self) -> Option<crate::users::Whose> {
        match self {
            Entry::Folder(folder) => folder.person,
            _ => None,
        }
    }

    /// The colour this row stands for — see [`Choice::swatch`].
    pub fn swatch(&self) -> Option<Color> {
        match self {
            Entry::Choice(choice) => choice.swatch,
            Entry::Bar(bar) => bar.swatch,
            _ => None,
        }
    }

    /// The material this row stands for, if it stands for one: see
    /// [`Choice::material`].
    pub fn material(&self) -> Option<Style> {
        match self {
            Entry::Choice(choice) => choice.material,
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
    /// The column with its rows left behind. See [`Column`].
    pub fn named(&self) -> Column {
        Column {
            id: self.id,
            title: self.title,
            icon: self.icon,
        }
    }

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

    // Last, because one row of it is a list of the columns there are — see
    // [`crate::settings::column`] — and until the retain above has run there is
    // no answer to that. The bar it is shown is the scanned columns and its own
    // place at the head of them, which is every column a machine has before an
    // account is signed in or a package puts one up; those arrive later and
    // rebuild this column when they do.
    let (id, title, icon) = SHELL_SETTINGS;
    let mut bar = vec![Column { id, title, icon }];
    bar.extend(categories.iter().map(Category::named));
    categories.insert(
        0,
        Category {
            id,
            title,
            icon,
            entries: crate::settings::column(&bar),
        },
    );
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

    // --- Software and Waydroid ----------------------------------------------

    /// An entry, as a machine really writes one.
    fn entry(file: &str, body: &str) -> App {
        App::parse(
            &format!("[Desktop Entry]\nType=Application\n{body}"),
            &Path::new("/usr/share/applications").join(file),
        )
        .unwrap_or_else(|| panic!("{file} should parse"))
    }

    /// The stores and the software hubs go in Software, whether or not they say
    /// the word the specification has for it.
    ///
    /// Discover is the case the known-store list exists for: it declares
    /// `Qt;KDE;System` and nothing else, so a rule that only read
    /// `PackageManager` would leave Plasma's own store filed with the disk
    /// utilities and the Software column on a Plasma machine empty.
    #[test]
    fn a_store_is_filed_under_software_however_it_describes_itself() {
        let declared = entry(
            "distribumpy.desktop",
            "Name=Software Hub\nExec=distribumpy\nCategories=System;PackageManager;\n",
        );
        assert_eq!(declared.category_id(), SOFTWARE);

        let known = entry(
            "org.kde.discover.desktop",
            "Name=Discover\nExec=plasma-discover\nCategories=Qt;KDE;System;\n",
        );
        assert_eq!(known.category_id(), SOFTWARE, "and it is not a system tool");

        // Case, because a `.desktop` file is written by hand and the
        // specification's own names are upper case by convention rather than by
        // rule.
        let shouted = entry(
            "bauh.desktop",
            "Name=bauh\nExec=bauh\nCategories=system;packagemanager;\n",
        );
        assert_eq!(shouted.category_id(), SOFTWARE);
    }

    /// A mod manager for one game is not a software hub, and the specification
    /// is what says so: `PackageManager` has to be paired with `System` or
    /// `Settings`, and CKAN pairs it with `Game`.
    ///
    /// Worth a test of its own because the loose reading of that rule is the
    /// obvious one, and it puts a Kerbal Space Program tool on the page
    /// somebody opens looking for their distribution's store.
    #[test]
    fn a_games_own_package_manager_stays_in_games() {
        let ckan = entry(
            "ckan.desktop",
            "Name=CKAN\nExec=ckan\nCategories=Game;PackageManager;\n",
        );
        assert_eq!(ckan.category_id(), GAMES);
    }

    /// Waydroid's entries carry one category nothing else on the machine uses,
    /// and no main category at all — so before this column they fell through to
    /// Other, which is where they were found.
    ///
    /// Waydroid's own launcher goes with them. It says `X-WayDroid-App;Utility`
    /// and would otherwise be filed under Utilities, one column away from
    /// everything it runs.
    #[test]
    fn every_entry_waydroid_wrote_is_in_the_waydroid_column() {
        let android = entry(
            "waydroid.com.termux.desktop",
            "Name=Termux\nExec=waydroid app launch com.termux\nCategories=X-WayDroid-App;\n",
        );
        assert_eq!(android.category_id(), WAYDROID);

        let launcher = entry(
            "Waydroid.desktop",
            "Name=Waydroid\nExec=waydroid\nCategories=X-WayDroid-App;Utility;\n",
        );
        assert_eq!(
            launcher.category_id(),
            WAYDROID,
            "the way in belongs with what it leads to"
        );

        // And a name that merely starts like Waydroid's is not Waydroid: the
        // category is what says so, not the file.
        let helper = entry(
            "com.jaoushingan.WaydroidHelper.desktop",
            "Name=Waydroid Helper\nExec=waydroid-helper\nCategories=Utility;\n",
        );
        assert_eq!(helper.category_id(), "utilities");
    }

    /// The entries Waydroid hides stay hidden. Nine of the seventeen on the
    /// machine this was written on carry `NoDisplay=true` — Android's own
    /// settings, its gallery, its clock — and they were hidden by the system
    /// that generated them rather than by this shell.
    #[test]
    fn a_hidden_android_entry_is_still_hidden() {
        assert!(App::parse(
            "[Desktop Entry]\nType=Application\nName=Settings\n\
             Exec=waydroid app launch com.android.settings\n\
             Categories=X-WayDroid-App;\nNoDisplay=true\n",
            Path::new("/tmp/waydroid.com.android.settings.desktop"),
        )
        .is_none());
    }

    /// The two new columns stand where they were asked to stand: Software after
    /// the libraries of games, and Waydroid at the end in front of Other.
    ///
    /// Asserted through [`rank`] rather than by building a bar, because that is
    /// the one place the order is written and every column that comes and goes
    /// is placed by it — see [`column_place`].
    #[test]
    fn the_two_new_columns_stand_where_they_belong() {
        let at = |id: &str| rank(id).unwrap_or_else(|| panic!("{id} is placeable"));
        assert!(at(GAMES) < at(STEAM.0));
        assert!(at(STEAM.0) < at(retroarch_column_id().0));
        assert!(
            at(retroarch_column_id().0) < at(SOFTWARE),
            "Software comes after everything that is a library of games"
        );
        assert!(at(SOFTWARE) < at("development"));
        assert!(at("utilities") < at(WAYDROID));
        assert!(at(WAYDROID) < at("other"), "and Other is still last");

        // And the same order, read off the whole set the Startup category page
        // is built from.
        let names: Vec<&str> = every_column().iter().map(|column| column.id).collect();
        let place = |id: &str| {
            names
                .iter()
                .position(|had| *had == id)
                .unwrap_or_else(|| panic!("{id} is a column"))
        };
        assert_eq!(place(SHELL_SETTINGS.0), 0, "the shell's own leads the bar");
        assert!(place(GAMES) < place(SOFTWARE));
        assert!(place(SOFTWARE) < place(WAYDROID));
        assert_eq!(place("other"), names.len() - 1);
    }

    /// An application can ask for its icon to be drawn in the shell's own
    /// material, in either of the two fields it is entitled to say it in.
    #[test]
    fn an_application_can_ask_for_the_shells_material() {
        let asked = entry(
            "distribumpy.desktop",
            "Name=Software Hub\nExec=distribumpy\nCategories=System;PackageManager;\n\
             Keywords=flatpak;store;lxb;\n",
        );
        assert!(asked.wears_shell_material());

        let said_it_the_other_way = entry(
            "thing.desktop",
            "Name=Thing\nExec=thing\nCategories=Utility;LXB;\n",
        );
        assert!(said_it_the_other_way.wears_shell_material());
        assert_eq!(
            said_it_the_other_way.category_id(),
            "utilities",
            "and the word is not a category anything is filed under"
        );

        // A word that merely contains it is not the word.
        let did_not = entry(
            "other.desktop",
            "Name=Other\nExec=other\nCategories=Utility;\nKeywords=lxbar;toolbox;\n",
        );
        assert!(!did_not.wears_shell_material());
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

    // --- RetroArch ----------------------------------------------------------

    /// One game out of somebody's own folder, as the shell holds it.
    fn rom(name: &str, startable: bool) -> Entry {
        Entry::Rom(Rom {
            name: name.to_string(),
            path: PathBuf::from(format!("/roms/psp/{name}.iso")),
            console: "PlayStation Portable".to_string(),
            note: "PlayStation Portable".to_string(),
            wanted: vec!["ppsspp".to_string()],
            boxart: None,
            snap: None,
            own_cover: false,
            own_background: false,
            shape: None,
            glyph: "lxb:console-psp".to_string(),
            start: startable.then(|| {
                vec![
                    "retroarch".to_string(),
                    "-L".to_string(),
                    "/usr/lib/libretro/ppsspp_libretro.so".to_string(),
                    format!("/roms/psp/{name}.iso"),
                ]
            }),
        })
    }

    /// A game deleted off the disk goes from the bar at once, exactly as a
    /// photograph does.
    ///
    /// The console's column is rebuilt from the helper's answer a moment later,
    /// so this is what takes the row off in the meantime — and a bar still
    /// offering to play a file the user has just watched themselves delete is a
    /// bar that has not understood.
    #[test]
    fn a_deleted_game_leaves_the_bar_with_its_file() {
        let mut categories = catalogue();
        offer_retroarch(&mut categories, Some("2 games".to_string()));
        shelve_retroarch(&mut categories, vec![rom("t8", true), rom("smb", true)]);
        let gone = std::path::PathBuf::from("/roms/psp/t8.iso");
        assert!(forget_file(&mut categories, &gone));
        let left: Vec<&str> = categories
            .iter()
            .flat_map(|column| &column.entries)
            .filter_map(Entry::rom)
            .map(|rom| rom.name.as_str())
            .collect();
        assert_eq!(left, ["smb"]);
        // And a second deletion of the same file changes nothing, which is what
        // says the walk is looking at the path rather than at a row number.
        assert!(!forget_file(&mut categories, &gone));
    }

    /// The RetroArch row goes *under* the Steam row, not above it: Steam is the
    /// row every session has and this one arrived with a package.
    #[test]
    fn the_retroarch_row_stands_under_the_steam_row() {
        let mut categories = catalogue();
        offer_steam(&mut categories, None);
        assert_eq!(
            offer_retroarch(&mut categories, Some("Looking for RetroArch".to_string())),
            Shifted::default(),
            "the Games column was already there"
        );

        let games = column(&categories, GAMES).expect("the Games column");
        let rows: Vec<&str> = games.entries.iter().map(Entry::title).collect();
        assert_eq!(rows, vec!["Steam", "RetroArch", "A Puzzle"]);
        assert_eq!(
            games.entries[1].comment(),
            Some("Looking for RetroArch"),
            "the row carries what it was given and nothing else"
        );

        // Rebuilt rather than added to, so a session that hears twice from its
        // helper has one row and not two.
        offer_retroarch(&mut categories, Some("Not installed".to_string()));
        let games = column(&categories, GAMES).expect("the Games column");
        let rows: Vec<&str> = games.entries.iter().map(Entry::title).collect();
        assert_eq!(rows, vec!["Steam", "RetroArch", "A Puzzle"]);

        // And nothing to say takes it off again, which is what a session whose
        // package has gone does.
        offer_retroarch(&mut categories, None);
        let games = column(&categories, GAMES).expect("the Games column");
        let rows: Vec<&str> = games.entries.iter().map(Entry::title).collect();
        assert_eq!(rows, vec!["Steam", "A Puzzle"]);
    }

    /// A session started with `--no-steam` has no Steam row for this one to
    /// stand under, and it goes at the head rather than under nothing.
    #[test]
    fn without_a_steam_row_it_stands_at_the_head() {
        let mut categories = catalogue();
        offer_retroarch(&mut categories, Some("Looking for RetroArch".to_string()));

        let games = column(&categories, GAMES).expect("the Games column");
        let rows: Vec<&str> = games.entries.iter().map(Entry::title).collect();
        assert_eq!(rows, vec!["RetroArch", "A Puzzle"]);
    }

    /// The column lands after Steam's and before whatever came after Games,
    /// which is the whole of what [`rank`] had to be re-solved for.
    #[test]
    fn the_column_lands_after_steam() {
        let mut categories = catalogue();
        offer_steam(&mut categories, Some("someone".to_string()));
        shelve_steam(&mut categories, vec![game(1, "A Game", true)]);
        let shifted = shelve_retroarch(&mut categories, vec![rom("t8", true)]);

        let at = categories
            .iter()
            .position(|column| column.id == retroarch_column())
            .expect("the RetroArch column");
        assert_eq!(shifted.added, Some(at));
        assert_eq!(
            categories[at - 1].id,
            STEAM.0,
            "it did not land after Steam"
        );
        assert_eq!(categories[at - 2].id, GAMES, "nor Steam after Games");
        assert!(
            categories[at + 1..]
                .iter()
                .any(|column| column.id == "development"),
            "everything the table puts after Games is still after it"
        );

        // And it goes away again with what was in it, like every other column
        // that is a consequence of something.
        let shifted = shelve_retroarch(&mut categories, Vec::new());
        assert_eq!(shifted.removed, Some(at));
        assert!(categories
            .iter()
            .all(|column| column.id != retroarch_column()));
    }

    /// A column of games is a column of covers whether or not there is a core
    /// to play them with, and a game that cannot be started is still the user's
    /// game.
    #[test]
    fn a_game_with_no_core_is_still_a_row() {
        let mut categories = catalogue();
        shelve_retroarch(&mut categories, vec![rom("t8", false)]);
        let games = column(&categories, retroarch_column()).expect("the column");
        let row = &games.entries[0];
        assert_eq!(row.title(), "t8");
        assert!(row.starts_something(), "it is a game, whatever runs it");
        assert!(
            row.rom().expect("a game").start.is_none(),
            "and there is nothing to start it with"
        );
    }

    /// RetroArch's own entry comes off the bar so that two rows do not say the
    /// same thing, and the shell's own row stands in its place.
    #[test]
    fn retroarchs_own_entry_comes_off_the_bar() {
        let mut categories = assemble(vec![
            App::parse(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec=/usr/bin/flatpak run org.libretro.RetroArch\nStartupWMClass=retroarch\nCategories=Game;Emulator;\n",
                Path::new("/usr/share/applications/org.libretro.RetroArch.desktop"),
            )
            .expect("a well-formed entry"),
            App::parse(
                "[Desktop Entry]\nType=Application\nName=A Puzzle\nExec=puzzle\nCategories=Game;\n",
                Path::new("/usr/share/applications/puzzle.desktop"),
            )
            .expect("a well-formed entry"),
        ]);

        assert!(hide_retroarch_client(&mut categories), "its own entry");
        let games = column(&categories, GAMES).expect("the Games column");
        let rows: Vec<&str> = games.entries.iter().map(Entry::title).collect();
        assert_eq!(rows, vec!["A Puzzle"], "its own entry is still on the bar");

        // A machine without RetroArch has nothing to take, and says so rather
        // than emptying a column.
        assert!(!hide_retroarch_client(&mut categories));
        assert!(column(&categories, GAMES).is_some());
    }

    /// Every row a folder chooser's column was opened from says so, which is
    /// how far the cursor comes back out when the question has been answered.
    #[test]
    fn a_pickers_own_columns_are_the_ones_it_comes_back_out_of() {
        let picking = crate::files::Shows::Folders(crate::settings::Picking::RomsFolder);
        let walking = |shows| {
            Entry::Folder(Folder {
                title: "Home".to_string(),
                comment: None,
                icon: None,
                entries: Vec::new(),
                place: Some(crate::files::Place::Directory(
                    PathBuf::from("/home"),
                    shows,
                )),
                chosen: false,
                over_the_list: false,
                person: None,
                portrait: None,
            })
        };

        // A folder of the walk, which is what every column of the chooser but
        // its first was opened from.
        assert!(opens_a_picker(&walking(picking)));
        // Its head, which is what the first was opened from: the same row,
        // standing under Settings and over the RetroArch column.
        assert!(opens_a_picker(&crate::retroarch::folder_row()));
        // The file explorer, which is the same walk for another reason and is
        // not one of these.
        assert!(!opens_a_picker(&walking(crate::files::Shows::Everything)));
        // And a page that merely *holds* the row, which is where the cursor
        // has to stop: Settings > Games > RetroArch is not a column of
        // somebody's disk.
        let page = Entry::Folder(Folder {
            title: "RetroArch".to_string(),
            comment: None,
            icon: None,
            entries: vec![crate::retroarch::folder_row()],
            place: None,
            chosen: false,
            over_the_list: false,
            person: None,
            portrait: None,
        });
        assert!(!opens_a_picker(&page));
        let games = crate::settings::Picking::RomsFolder;
        assert_eq!(picker_row_for(std::slice::from_ref(&page), games), None);
        assert_eq!(
            picker_row_for(page.entries().expect("its rows"), games),
            Some(0)
        );
        assert_eq!(
            picker_row_for(
                page.entries().expect("its rows"),
                crate::settings::Picking::Firmware
            ),
            None,
            "and a press meant for the BIOS does not open the games folder"
        );
    }

    /// The row at the head of a folder picker stands *over* the column, so the
    /// column opens on the folder below it and a press of A out of habit does
    /// not answer a question nobody has read.
    #[test]
    fn the_picker_never_opens_on_the_row_that_answers_it() {
        let rows = place_rows(
            vec![Entry::Folder(Folder {
                title: "psp".to_string(),
                comment: None,
                icon: None,
                entries: Vec::new(),
                place: None,
                chosen: false,
                over_the_list: false,
                person: None,
                portrait: None,
            })],
            "",
            1,
            Some(Pick {
                at: PathBuf::from("/home/someone/ROMs"),
                about: crate::settings::Picking::RomsFolder,
                comment: "Look for games in this folder".to_string(),
            }),
            // And no New folder row either, for the same reason there is no
            // field: a walk that is choosing a folder is not one where a brand
            // new empty one could be the answer. See
            // [`crate::files::can_make_a_folder`].
            None,
        );
        assert_eq!(rows[0].title(), "Select folder");
        assert!(rows[0].over_the_list(), "it stands over the list");
        assert_eq!(head_rows(&rows), 1, "so the column opens below it");
        assert!(
            rows[1].search().is_none() && rows[0].search().is_none(),
            "and there is no field: the rows are not what the user came for"
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
                over_the_list: false,
                person: None,
                portrait: None,
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
                over_the_list: false,
                person: None,
                portrait: None,
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
                Some(crate::files::Place::Volumes(
                    crate::files::Shows::Everything
                )),
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
