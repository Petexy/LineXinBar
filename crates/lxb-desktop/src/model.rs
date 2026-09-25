//! The lattice navigation model.
//!
//! The lattice is two orthogonal lists: categories run horizontally,
//! and the selected category's items run vertically through it. Selection is
//! integer, but the drawn position is a float that eases towards it, which is
//! what gives the interface its characteristic glide.
//!
//! A column is a tree rather than a list. Stepping right into a subcategory
//! opens a column of its own beside the one it came from, and the bar slides
//! over to it — the same move the original made, and the reason its Settings
//! region could be a handful of rows deep without ever being a long list. The
//! path the user has taken is therefore a *stack* of columns, and what makes
//! the move legible is that the columns behind keep the row they were opened
//! from: a trail reading left to right, category above it.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::apps::{App, Category, Entry};
use crate::settings::Setting;

/// How stiff the spring the bar rides is, in radians per second. Higher is
/// snappier. Critically damped like the cards, so a move leans in and settles
/// rather than bolting off the mark, and holding a direction carries the
/// bar's momentum through each step instead of restarting it.
const EASE_RATE: f32 = 19.0;

/// The same for the step in and out of a subcategory, which is gentler.
///
/// Not a matter of taste: a spring rate is in units per second, and one unit
/// of depth is a wider journey across the screen than one unit of category —
/// the column it opens has to stand clear of the one it came from, labels and
/// all. Driven at the row's own rate it would arrive faster than anything else
/// the bar does, which reads as a jump rather than as a move.
const DEPTH_EASE_RATE: f32 = 14.0;

/// Below this distance the animation is finished and we stop redrawing.
const SETTLED: f32 = 0.001;

/// The same, at `target` — which is [`SETTLED`] everywhere a float has the
/// resolution to say so, and a few of its own steps where it has not.
///
/// A position here is counted in rows, and a shelf of the user's own files is
/// as long as their home directory is full. Scrolled a thousand rows down one,
/// an `f32` cannot hold two positions a thousandth of a row apart at all: the
/// spring arrives one representable step short of its target, cannot move
/// again, and its velocity settles on a small number instead of on nothing —
/// so the bar was never finished, and the shell went on drawing sixty frames a
/// second, for ever, over a distance a hundredth the width of a pixel. It is
/// the one place where "close enough" has to be asked in the units the number
/// is actually kept in.
///
/// Thirty-two steps, which is three times the worst a stalled spring holds on
/// to at any frame rate, and still a fraction of a pixel at any position a bar
/// can be scrolled to.
fn settled_within(target: f32) -> f32 {
    SETTLED.max(target.abs() * 32.0 * f32::EPSILON)
}

/// Which row of `entries` a column opens on, and comes back to rest on when it
/// is reordered: the first that belongs to the list rather than standing over
/// it. Zero for every column carrying no such row, which is all but the three
/// shelves of the user's own files.
fn first_row(entries: &[Entry]) -> usize {
    crate::apps::head_rows(entries).min(entries.len().saturating_sub(1))
}

/// How a terminal emulator separates its own options from the program it
/// should run. Unfortunately there is no universally implemented CLI here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalExecStyle {
    DashE,
    DoubleDash,
    Execute,
    Konsole,
    WezTerm,
}

/// Fallbacks used only when `$TERMINAL` is absent, malformed, or unavailable.
/// `xdg-terminal-exec` comes first because it resolves the user's configured
/// default from Desktop Entries. Konsole is next so a stock Arch/KDE system
/// works without Debian's `x-terminal-emulator` alternatives mechanism.
const TERMINAL_CANDIDATES: &[(&str, TerminalExecStyle)] = &[
    ("xdg-terminal-exec", TerminalExecStyle::DoubleDash),
    ("konsole", TerminalExecStyle::Konsole),
    ("foot", TerminalExecStyle::DashE),
    ("ghostty", TerminalExecStyle::DashE),
    ("kgx", TerminalExecStyle::DoubleDash),
    ("ptyxis", TerminalExecStyle::DoubleDash),
    ("gnome-terminal", TerminalExecStyle::DoubleDash),
    ("alacritty", TerminalExecStyle::DashE),
    ("kitty", TerminalExecStyle::DoubleDash),
    ("wezterm", TerminalExecStyle::WezTerm),
    ("xfce4-terminal", TerminalExecStyle::Execute),
    ("mate-terminal", TerminalExecStyle::Execute),
    ("qterminal", TerminalExecStyle::DashE),
    ("terminator", TerminalExecStyle::Execute),
    ("x-terminal-emulator", TerminalExecStyle::DashE),
    ("xterm", TerminalExecStyle::DashE),
    ("urxvt", TerminalExecStyle::DashE),
    ("st", TerminalExecStyle::DashE),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Left,
    Right,
    Up,
    Down,
    Launch,
    /// Start — Accept everywhere except over the on-screen keyboard, where it
    /// is the one press that finishes typing: Enter, and the board away.
    ///
    /// It is folded into [`Action::Launch`] on its way in whenever the board is
    /// not up (see `Shell::on_action`), because that is the only place the two
    /// differ and nothing behind the board should have to know there are two.
    /// A field being typed into ends with Enter and then with the keyboard
    /// gone, which on a pad is two presses in two different places — the Enter
    /// key at one end of the board and the key that folds it away at the other
    /// — and Start is the button every console has already taught for it.
    Submit,
    /// Step back out of wherever the user is. Never quits on its own: leaving
    /// the session is an explicit choice in the guide menu.
    Back,
    /// Summon or dismiss the guide overlay. Unlike every other action this one
    /// is honoured even while an application owns input, because it is the way
    /// back out of that application.
    Guide,
    /// Summon or dismiss the on-screen keyboard. Honoured from outside for the
    /// same reason as [`Action::Guide`]: the application the letters are meant
    /// for is the one holding the keys.
    Keyboard,
    /// Raise or dismiss the context menu on whatever is selected — the short
    /// list of things that can be done to it, as against the one thing Accept
    /// does. Deliberately *not* honoured from outside: it is about something
    /// the shell is showing, and while an application owns the screen there is
    /// nothing of the shell's for it to be about.
    Menu,
    /// Raise or dismiss the Steam friends list: who the account knows, and
    /// where each of them is, on a column down the right of the screen.
    ///
    /// The left-hand face button, and Shift on a keyboard. Deliberately *not*
    /// honoured from outside, for the reason [`Action::Menu`] is not: it is one
    /// of the shell's own screens, and a game holding the display is a game
    /// whose own left-hand face button this would be taking.
    ///
    /// It takes every direction while it is up. That is the whole of what makes
    /// it a panel rather than a second thing on the same screen: the list is a
    /// column of names on the right and the bar is a column of rows on the
    /// left, and a press that could have meant either would mean neither.
    Friends,
    /// Hand control to the previous / next display.
    PrevScreen,
    NextScreen,
    /// Photograph the whole of the display being driven.
    ///
    /// Honoured from outside like [`Action::Guide`] and [`Action::Keyboard`],
    /// and for a stronger reason than either: what a person wants a picture of
    /// is almost always the game holding the screen, so an action that only
    /// worked on the shell's own screens would work everywhere except where it
    /// is wanted.
    Screenshot,
    /// Announce a message from whoever is selected on the friends panel, as
    /// though one had arrived.
    ///
    /// **`--debug-actions message` only, and nothing sends it.** A chat toast
    /// cannot be photographed on demand any other way: it needs somebody else
    /// to write, and the one thing this shell cannot arrange is another person.
    /// So this raises the announcement — the sender's name, their face, and a
    /// body — through the one function that raises a real one, and reaches
    /// Steam not at all.
    PretendAMessage,
    /// The same for an invitation to a game, from whoever the panel is standing
    /// on.
    ///
    /// **`--debug-actions invite` only, and nothing is sent or joined.** An
    /// invitation needs another person even more than a message does — somebody
    /// has to be *in a game* and press Invite — and what it puts on the screen
    /// is the announcement and the card in the conversation. The lobby is
    /// invented, so accepting it reaches Valve's client with a lobby that does
    /// not exist; what can be seen is the card, the legend, the press and the
    /// loading screen.
    PretendAnInvite,
    /// Two agreements for a game, put up on the panel a real install puts
    /// them on.
    ///
    /// **`--debug-actions agreement` only, and nothing is accepted or
    /// installed.** The real one needs a game the account owns, has never
    /// accepted the terms of, and is willing to download; this needs none of
    /// that. Its words are invented, and its Accept steps to the second
    /// agreement and then puts the panel away, recording nothing and reaching
    /// Steam not at all. What can be seen is the panel, the well, the
    /// scrolling and the step from one agreement to the next.
    PretendAnAgreement,
    /// Put the panel that asks which Steam library a game goes into on the
    /// screen, with three invented libraries on it.
    ///
    /// **`--debug-actions library` only, and nothing is installed.** The real
    /// one needs a machine with two Steam libraries and a game the account
    /// owns and has not got; this needs neither. One of the three is too small
    /// for the invented game, so the button that cannot be pressed is seen
    /// too, and choosing any of the others puts the panel away.
    PretendALibraryChoice,
    /// Ask Valve's own overlay to come up over the game in front.
    ///
    /// Honoured from outside for the plainest reason of the four: there is
    /// nowhere else it can be asked from. A game holding the screen is the
    /// only state this means anything in.
    ///
    /// It exists because the shell takes the guide button, which is the button
    /// Steam's overlay is opened with on every other console-shaped machine.
    /// Taking a control away and putting nothing in its place would be this
    /// shell deciding that nobody may reach Steam's friends list, its browser
    /// or its guides while playing — which is not a decision a shell gets to
    /// make about somebody else's application. So the button is still the
    /// shell's, and the overlay is a chord on it.
    SteamOverlay,
    /// Hand the guide's own directions to the videos floating over it, and take
    /// them back.
    ///
    /// The right stick pressed. It is the one control on a pad that is free at
    /// exactly the moment this is wanted: while the guide is up the stick is not
    /// aiming a pointer — there is nothing of an application's for it to aim at
    /// — and while it *is* aiming one this shell is not on screen to be asked.
    ///
    /// Not honoured from outside, for the reason [`Action::Menu`] is not: it is
    /// about something the shell is showing, and a video floating over a game
    /// with no overlay up is a window the user is deliberately not being offered
    /// anything to do to.
    Floating,
    /// Turn the session up or down by one step, or silence it.
    ///
    /// Honoured from outside for the same reason again, and the plainest of
    /// the four: the application holding the screen is the thing being turned
    /// down. Nothing on screen is opened, closed or moved by these — they set
    /// the machine's own volume and say on screen that they did, wherever the
    /// user happens to be.
    VolumeUp,
    VolumeDown,
    VolumeMute,
}

/// The applications available to launch, and the processes started from them.
///
/// Shared by every display: what each one is *pointing at* lives in its own
/// [`Cursor`], but there is only one catalogue and one set of running children.
pub struct Lattice {
    pub categories: Vec<Category>,

    /// The applications taken off the bar, which no column draws and the rest
    /// of the shell still knows about.
    ///
    /// One of them at the time of writing: *Pictures*, which has no tile
    /// because it is reached from the menu over the shelf of photographs it is
    /// for. It is still what a photograph opens in, still on the Open with
    /// list, and still the answer to "what handles `image/png`" — which is why
    /// it is set aside rather than dropped. See
    /// [`crate::apps::take_off_the_bar`].
    pub aside: Vec<crate::apps::App>,

    /// Socket selected before connecting the shell. Child applications are
    /// pinned to the same socket instead of inheriting a nested host display.
    wayland_display: OsString,
    /// LineXinBar's private XWayland display, when the compositor supplied one.
    /// An arbitrary inherited `DISPLAY` is never trusted here.
    xwayland_display: Option<OsString>,
    /// Direct children retained only so their exit status can be collected.
    /// Dropping a [`Child`] never terminates the process, so applications still
    /// outlive the shell if LineXinBar itself exits first.
    launched_apps: Vec<LaunchedApp>,
}

/// Where one display's bar is pointing.
///
/// Each display owns one, which is what makes several displays separate bars
/// rather than copies of the same one: they browse independently, and only the
/// focused display responds to input.
#[derive(Debug, Clone)]
pub struct Cursor {
    /// Index of the focused category.
    pub selected_category: usize,
    /// Focused item *per category*, so moving away and back returns you to
    /// where you were, as the console shells this follows do.
    ///
    /// `None` for a column this cursor has never stood in, which is not the
    /// same as row zero of it. A display is in one column at a time and has no
    /// opinion about the rest of the bar; a row it never chose is a place to
    /// start from when it first walks in, and not a row it is standing on. What
    /// turns on the difference is [`Self::row_in_column`].
    selected_items: Vec<Option<usize>>,

    /// The subcategories stepped into, outermost first.
    ///
    /// Longer than `open` by at most one, and that one is the column just
    /// stepped out of: it is kept so it can be *watched* leaving rather than
    /// blinking out, and so that stepping straight back in returns to the row
    /// it was left on rather than to the top of a column that never moved.
    /// Anything that makes it unreachable — a move up or down the column it
    /// hangs off, a change of category — drops it there and then.
    stack: Vec<SubColumn>,
    /// How many of `stack` the user is actually standing in. Zero is the
    /// category's own column.
    open: usize,

    /// Eased position of the category bar, in category units.
    pub category_position: f32,
    /// Eased position of the item column, in item units.
    pub item_position: f32,
    /// Eased depth, in columns. The whole cross rides this, which is what
    /// makes stepping into a subcategory one move of the bar rather than one
    /// list being swapped for another.
    depth_position: f32,
    /// How fast each of those is moving, in units per second. Held because a
    /// spring needs it: it is what makes the bar lean into a move instead of
    /// leaving at full speed, and what carries the momentum of a held
    /// direction from one step into the next rather than starting over.
    category_speed: f32,
    item_speed: f32,
    depth_speed: f32,
}

/// Read the place on `entries[row]` off the disk, keeping what answers to
/// `query`, and hang what comes back under it.
///
/// The one place a folder's column is built, so opening one and searching one
/// cannot come to different conclusions about what is in it. `None` for a row
/// that does not stand for anywhere, which is every row in the shell but the
/// explorer's; otherwise what the listing could be ordered by, which is a
/// question about the filesystem it came off and is therefore only answerable
/// by the read that just happened.
fn read_into(
    entries: &mut [Entry],
    row: usize,
    query: &str,
    how: crate::files::How,
) -> Option<crate::media::Orders> {
    let Some(Entry::Folder(folder)) = entries.get_mut(row) else {
        return None;
    };
    let place = folder.place.clone()?;
    let shown = match place {
        crate::files::Place::Volumes(shows) => crate::files::volumes(query, shows),
        crate::files::Place::Directory(at, shows) => crate::files::listing(&at, query, how, shows),
        // No query: the trash carries no search field, for the reasons
        // [`crate::files::trash`] gives. What is passed here would be the
        // query of whatever column happened to be searched last.
        crate::files::Place::Trash => crate::files::trash(how.sort),
        // Nothing to read until it is mounted, which is a press rather than a
        // read — see `Shell::open_the_unmounted_drive`.
        crate::files::Place::Unmounted(_) => return None,
    };
    folder.entries = shown.rows;
    // What was found, in place of the date the row was carrying: "14 folders,
    // 6 files" is what the user has just been shown, and a row that went on
    // saying when the folder was last written would be answering a question
    // nobody asked twice. What a *search* has left of it is on the field
    // instead, which is the row that is doing the narrowing.
    folder.comment = Some(shown.note);
    folder.comment_message = None;
    Some(shown.orders)
}

/// Where Files is on this bar: the category holding it, and the row that opens
/// the disks.
///
/// Found by what the row *is* and not by what it is called. The Files row is
/// the one entry in the tree standing for the whole disk as it is —
/// `Place::Volumes(Shows::Everything)` — and the other rows that open a list of
/// disks are all somebody being asked to *choose* something: a wallpaper, a
/// face, where a console's games are. Each carries what it is choosing, so none
/// of them can be walked into by mistake, whatever any category on this machine
/// happens to be titled. See [`crate::files::Shows`].
fn the_files_row(categories: &[Category]) -> Option<(usize, usize)> {
    categories.iter().enumerate().find_map(|(at, column)| {
        let row = column.entries.iter().position(|entry| {
            matches!(
                entry,
                Entry::Folder(folder)
                    if folder.place
                        == Some(crate::files::Place::Volumes(crate::files::Shows::Everything))
            )
        })?;
        Some((at, row))
    })
}

/// Which row of `entries` leads to `path`.
///
/// By where a row goes rather than by what it is called: two rows in one column
/// can read the same — a folder and a file beside it, a name that differs only
/// in case — and a walk that matched on names would step into whichever came
/// first.
fn row_leading_to(entries: &[Entry], path: &Path) -> Option<usize> {
    entries.iter().position(|entry| match entry {
        Entry::Folder(folder) => {
            matches!(folder.place.as_ref(), Some(crate::files::Place::Directory(at, _)) if at == path)
        }
        Entry::File(file) => file.path == path,
        _ => false,
    })
}

/// The disk in `entries` that holds `path`, deepest first.
///
/// `$HOME` is inside `/`, so both rows hold a file in the user's own folder and
/// only one of them is where anybody would look for it. The picker's own rule,
/// and the same one word for word; see [`crate::picker::Picker::walk_to`].
fn deepest_disk_holding(entries: &[Entry], path: &Path) -> Option<PathBuf> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Folder(folder) => match folder.place.as_ref() {
                Some(crate::files::Place::Directory(at, _)) => Some(at),
                _ => None,
            },
            _ => None,
        })
        .filter(|at| path.starts_with(at))
        .max_by_key(|at| at.components().count())
        .cloned()
}

/// One column below the category's own.
#[derive(Debug, Clone)]
struct SubColumn {
    selected: usize,
    /// Eased row, in item units — its own, so a column keeps gliding where it
    /// was left while the user is a level away from it.
    position: f32,
    speed: f32,
}

/// Where a cursor was standing before the bar was rebuilt underneath it.
///
/// A column's name is the one thing about it that a rebuild cannot change: the
/// bar is read off the disk again, the columns come back in whatever order the
/// disk put them, and the Steam library and the RetroArch column are hung on
/// afterwards — so every number a cursor holds is about a bar that no longer
/// exists. This is the same account of where somebody is standing, written in
/// names. See [`Cursor::remember`], which takes one, and [`Cursor::recall`],
/// which is the only thing that reads one.
#[derive(Debug, Clone)]
pub struct Footing {
    /// The column the cursor was in, by name. `None` only for a cursor that was
    /// standing outside the bar altogether, which is a bar with nothing on it.
    standing: Option<&'static str>,
    /// And where that column was, for the one case a name cannot answer: the
    /// column itself has gone, and what is wanted is whichever column closed
    /// the gap.
    was: usize,
    /// What each column had been left on, under the name it goes by.
    remembered: Vec<(&'static str, Option<usize>)>,
    /// How far the bar was from the column it was travelling to, and how fast.
    /// Kept so that a bar rebuilt mid-stride goes on walking rather than
    /// arriving early.
    drift: f32,
    speed: f32,
    /// The path the user had opened, and how much of it they were standing in.
    stack: Vec<SubColumn>,
    open: usize,
    /// And what that path was *of*, for the one column whose columns are read
    /// off the disk rather than assembled: the explorer's. Every other column
    /// on the bar comes back from the scan with its subcategories under it, so
    /// putting the stack back is enough. A folder does not — a scan finds the
    /// Files row with nothing but the disks under it — so the rows a rebuilt
    /// bar has for an open folder are none at all, and the cursor is clamped
    /// straight back out to the top of the column. These are the places to open
    /// again to put somebody back where they were standing. Empty for every
    /// column but that one. See [`Cursor::walk_back_in`].
    walked: Vec<crate::files::Place>,
}

/// One column of the item list as the bar is currently showing it.
///
/// What [`build`] draws from: the columns behind the open one are still on
/// screen, keeping the row they were opened from, so the trail is part of the
/// view rather than something the shell remembers separately.
///
/// [`build`]: crate::ui::build
pub struct Column<'a> {
    pub entries: &'a [Entry],
    /// The row the cursor is on in this column.
    pub selected: usize,
    /// Eased row position, in item units.
    pub position: f32,
    /// Where this column stands in relation to the cursor.
    pub standing: Standing,
}

/// Where a column of the path stands in relation to the cursor.
///
/// The drawing needs this because a step in and a step out are not each
/// other's mirror. Whichever way the bar is going, one column is arriving into
/// the space in front of it and the rest are giving that space up — and it is
/// the giving up that has to be quick, because those rows are laid over the
/// ones taking their place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The column the cursor is in. Whole, lit, and the one thing on screen
    /// that is never on its way anywhere.
    Open,
    /// One the path runs *through*: opened, and then stepped past. It keeps
    /// the row it was opened from — the trail — and gives up the rest.
    Behind,
    /// The one just stepped back out of, kept for as long as it takes to
    /// leave so it can be watched going rather than blinking out.
    Leaving,
}

struct LaunchedApp {
    name: String,
    command: String,
    child: Child,
    started_at: Instant,
    wait_error_reported: bool,
    /// The game this process is playing, where it is playing one.
    ///
    /// Everything else the bar starts is watched only to be tidied away. A
    /// game is watched to be *answered*: an emulator that comes straight back
    /// has told the shell something, and until this was here the only place it
    /// said it was the log. See [`Played`].
    played: Option<Played>,
    /// Whether this is Heroic's courier for an Epic game. Started cold, it
    /// lives as long as the game and a second more — Heroic writes the
    /// playtime and exits — so its going is the moment the library on the
    /// disk has something new in it. See [`Lattice::running_through_heroic`].
    through_heroic: bool,
}

/// One game a launched process was playing, and how quickly it stopped.
///
/// What the shell needs to answer a start that failed. Carried on the process
/// rather than looked up afterwards, for the reason the argv is carried on the
/// row: by the time it exits the cursor has very likely moved, and the answer
/// has to be about the game that was pressed.
#[derive(Debug, Clone)]
pub struct Played {
    /// The game, by the path its row is held under.
    pub rom: PathBuf,
    /// The console it came out of, under the name the shell calls it.
    pub console: String,
    /// How long it lasted.
    pub lasted: Duration,
}

impl Lattice {
    /// Build a model for the process's named Wayland display.
    ///
    /// The shell startup path uses [`Self::with_wayland_display`] after
    /// validating the environment. This convenience constructor keeps model
    /// consumers that never launch an application lightweight.
    #[cfg(test)]
    pub fn new(categories: Vec<Category>) -> Self {
        Self::with_wayland_display(
            categories,
            std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| OsString::from("wayland-0")),
        )
    }

    #[cfg(test)]
    pub fn with_wayland_display(categories: Vec<Category>, wayland_display: OsString) -> Self {
        Self::with_session_displays(categories, wayland_display, None)
    }

    pub fn with_session_displays(
        categories: Vec<Category>,
        wayland_display: OsString,
        xwayland_display: Option<OsString>,
    ) -> Self {
        Self {
            categories,
            aside: Vec::new(),
            wayland_display,
            xwayland_display,
            launched_apps: Vec::new(),
        }
    }

    /// Whether there is nothing here to launch.
    ///
    /// Not the same as having no columns, nor even as having no rows: the
    /// shell's own Settings column is always present and is full of rows that
    /// start no process at all. An empty catalogue is one with nothing
    /// anywhere in it that a press would start — no application and none of
    /// the user's own music or films — subcategories included.
    pub fn is_empty(&self) -> bool {
        !self.categories.iter().any(Category::has_launchable)
    }

    /// Start whatever `cursor` is pointing at. Returns the process id of what
    /// it started — `None` if there was nothing there, or if it would not
    /// start, which the caller needs to tell apart from a slow launch before
    /// it puts a splash up over one that is never coming.
    pub fn launch_selected(&mut self, cursor: &Cursor) -> Option<u32> {
        if let Some(file) = cursor.current_entry(self).and_then(Entry::media) {
            return self.open_media(&file.clone());
        }
        // The same thing in the other column it can be pressed from. One
        // function underneath both, so a song opened out of the folder it is
        // in joins the launched applications on exactly the terms it would
        // have from the shelf — under its own name, and closable like
        // anything else.
        if let Some(file) = cursor.current_entry(self).and_then(Entry::file) {
            let file = file.clone();
            return self.open_file(&file);
        }
        // A game out of somebody's own ROM folder, which carries the whole
        // command line that starts it — see [`crate::apps::Rom::start`]. It is
        // filed under the *game's* name like the two above, and for the same
        // reason: what the user pressed was a game, and a guide offering to
        // close "RetroArch" would be about a program they never chose to think
        // about.
        if let Some(rom) = cursor.current_entry(self).and_then(Entry::rom) {
            let rom = rom.clone();
            return self.play_rom(&rom);
        }
        // A game of the Epic account, which carries Heroic's own shortcut for
        // it — see [`crate::apps::EpicGame::start`] — and is filed under the
        // game's name for the reason the two above are.
        if let Some(game) = cursor.current_entry(self).and_then(Entry::epic_game) {
            let game = game.clone();
            return self.play_epic(&game);
        }
        let app = cursor.current_app(self)?;
        let name = app.name.clone();
        let entry = app.path.clone();
        let command = app.exec.clone();
        let terminal = app.terminal;
        // The one entry on this bar that is not simply a reader of controllers.
        // It is only ever here on a session whose Steam integration is off —
        // with it on, the shell's own row replaces this one and the client is
        // started by the integration, which asks the same question its own way.
        // Asked by the same test that takes the entry off the bar in the other
        // case, so the two cannot come to different conclusions about which
        // row is Valve's. See [`crate::apps::hide_steam_client`].
        let pads = match app.owns_window("steam") {
            true => Pads::ForValvesClient,
            false => Pads::ForAnApplication,
        };
        tracing::info!(app = %name, entry = %entry.display(), "launching");

        match launch(
            &command,
            terminal,
            pads,
            &self.wayland_display,
            self.xwayland_display.as_deref(),
        ) {
            Ok(child) => {
                let pid = child.id();
                tracing::info!(app = %name, command, pid, "application process started");
                self.launched_apps.push(LaunchedApp {
                    name,
                    command,
                    child,
                    started_at: Instant::now(),
                    wait_error_reported: false,
                    played: None,
                    through_heroic: false,
                });
                Some(pid)
            }
            Err(err) => {
                tracing::warn!(app = %name, command, ?err, "failed to start application");
                None
            }
        }
    }

    /// Start one of the applications the bar does not carry.
    ///
    /// Launched on exactly the terms a tile's press launches one: the same
    /// command line, the same environment, and filed in `launched_apps` under
    /// its own name so the guide can close it like anything else. The only
    /// difference is where the press came from — a row on the menu over a
    /// shelf, rather than a tile — and that is not a difference the launch
    /// should be able to tell.
    ///
    /// No file is handed to it. The application this is for opens the user's
    /// pictures folder when it is given nothing, which is the whole of what
    /// the row means.
    pub fn launch_aside(&mut self, app_id: &str) -> Option<u32> {
        let app = self.aside.iter().find(|app| app.owns_window(app_id))?;
        let name = app.name.clone();
        let command = app.exec.clone();
        let terminal = app.terminal;
        tracing::info!(app = %name, command, "launching an application that is not on the bar");

        match launch(
            &command,
            terminal,
            Pads::ForAnApplication,
            &self.wayland_display,
            self.xwayland_display.as_deref(),
        ) {
            Ok(child) => {
                let pid = child.id();
                tracing::info!(app = %name, command, pid, "application process started");
                self.launched_apps.push(LaunchedApp {
                    name,
                    command,
                    child,
                    started_at: Instant::now(),
                    wait_error_reported: false,
                    played: None,
                    through_heroic: false,
                });
                Some(pid)
            }
            Err(err) => {
                tracing::warn!(app = %name, command, ?err, "failed to start application");
                None
            }
        }
    }

    /// Start one game out of somebody's ROM folder.
    ///
    /// The argv is on the row, so nothing is asked of anything here: see
    /// [`crate::apps::Rom::start`], which is where it was built and why. A row
    /// with none is a console with no core installed, and its press is answered
    /// by a panel rather than by this — `Shell::say_no_core`.
    fn play_rom(&mut self, rom: &crate::apps::Rom) -> Option<u32> {
        let argv = rom.start.as_ref()?;
        let command = crate::retroarch::shell_command(argv);
        tracing::info!(game = %rom.name, file = %rom.path.display(), "playing");
        self.open_path_with(
            &rom.path,
            &rom.name,
            crate::media::Opening {
                name: "RetroArch".to_string(),
                // Nothing offers this one off a list, so there is no row to
                // draw a picture on: the splash is the game's, wearing the
                // mark of the column it came out of.
                icon: None,
                command,
            },
            // The one launch on this bar whose failure is answered rather than
            // only logged. See [`Played`].
            Some(Played {
                rom: rom.path.clone(),
                console: rom.console.clone(),
                lasted: Duration::ZERO,
            }),
        )
    }

    /// Start one game of the Epic account, through Heroic.
    ///
    /// A row with no command is a game not on this disk, and its press is
    /// answered before it gets here.
    pub fn play_epic(&mut self, game: &crate::apps::EpicGame) -> Option<u32> {
        let argv = game.start.as_ref()?;
        tracing::info!(game = %game.name, app = %game.app_name, "playing, through Heroic");
        let pid = self.open_command(crate::media::Opening {
            name: game.name.clone(),
            icon: None,
            command: crate::retroarch::shell_command(argv),
        })?;
        if let Some(launched) = self.launched_apps.last_mut() {
            launched.through_heroic = true;
        }
        Some(pid)
    }

    /// Since when a Heroic this shell started has been running, where one is:
    /// the oldest of its couriers still alive. See [`crate::heroic::Heroic::landed_at`].
    pub fn heroic_running_since(&self) -> Option<Instant> {
        self.launched_apps
            .iter()
            .filter(|app| app.through_heroic)
            .map(|app| app.started_at)
            .min()
    }

    /// How many of Heroic's couriers are still running — see
    /// [`LaunchedApp::through_heroic`]. The shell compares it across a reap.
    pub fn running_through_heroic(&self) -> usize {
        self.launched_apps
            .iter()
            .filter(|app| app.through_heroic)
            .count()
    }

    /// Start one command line the shell built itself, under a name of its own.
    ///
    /// Play one of the user's own files, in whatever they have chosen to open
    /// that kind of file with.
    ///
    /// The player joins the launched applications like anything else the bar
    /// starts, under the *file's* name rather than its own: what the user
    /// pressed was a song, so that is what the guide should say is running and
    /// what Close should offer to end. The program behind it is in the log,
    /// which is where the question "why did that open in VLC" is answered.
    fn open_media(&mut self, file: &crate::media::File) -> Option<u32> {
        let opening = crate::media::opening(&file.path, file.mime, &self.categories, &self.aside)?;
        self.open_media_with(file, opening)
    }

    /// Open whatever `cursor` is pointing at in the application whose desktop
    /// entry is called `entry` — `vlc.desktop` and the like.
    ///
    /// What the Open with list's *Other application* half comes down to. The
    /// ordinary Open asks the desktop which program handles the type and takes
    /// the answer; this is the press for the case where there is no answer to
    /// take, which is every file with no extension on it.
    ///
    /// Only the two kinds of row that are files. A row that is not one is not
    /// something an application can be handed, and the list is never raised
    /// over one — but it is answered here as well as there, because the menu
    /// that offered the row and the press that arrives are two separate
    /// moments and the bar underneath goes on living between them.
    ///
    /// Nothing is written down. The file opens in what was chosen and the
    /// desktop's own answer for the type is left exactly as it was; making the
    /// choice stick is a second press on a row of its own, which is what
    /// [`crate::menu::Command::OpenWithAlways`] is for.
    pub fn open_selection_with(&mut self, cursor: &Cursor, entry: &str) -> Option<u32> {
        let (path, title) = match cursor.current_entry(self)? {
            Entry::Media(file) => (file.path.clone(), file.title.clone()),
            Entry::File(file) => (file.path.clone(), file.name.clone()),
            _ => return None,
        };
        // The borrow of the catalogue ends with this line: what comes out is
        // an owned command line, and starting it needs the catalogue mutably.
        let opening = crate::media::opening_with(
            &path,
            crate::media::entry_named(entry, &self.categories, &self.aside)?,
        );
        tracing::info!(
            file = %path.display(),
            with = %opening.name,
            "opening this in an application the user picked by name"
        );
        self.open_path_with(&path, &title, opening, None)
    }

    /// The same, for a file the explorer found in a folder.
    ///
    /// Its own two lines rather than a shared one, because the two rows carry
    /// different things and the difference is the whole of what the user
    /// sees: a shelved song is titled without its extension and this is titled
    /// with it, which is what the column it was pressed in is *for*.
    fn open_file(&mut self, file: &crate::files::Item) -> Option<u32> {
        let opening = crate::media::opening(&file.path, file.mime, &self.categories, &self.aside)?;
        self.open_path_with(&file.path, &file.name, opening, None)
    }

    /// The same, in an application the user has picked by name off the Open
    /// with list rather than the one their desktop already answers with.
    ///
    /// One function underneath both, so a file opened either way joins the
    /// launched applications on identical terms — under the file's name, and
    /// closable like anything else. The only difference between the two is
    /// which command line got here.
    pub fn open_media_with(
        &mut self,
        file: &crate::media::File,
        opening: crate::media::Opening,
    ) -> Option<u32> {
        self.open_path_with(&file.path, &file.title, opening, None)
    }

    /// What both of those come down to: start `opening`, and file the process
    /// under the name of the file rather than the name of the program.
    fn open_path_with(
        &mut self,
        path: &Path,
        title: &str,
        opening: crate::media::Opening,
        played: Option<Played>,
    ) -> Option<u32> {
        tracing::info!(
            file = %path.display(),
            with = %opening.name,
            "opening a file the shell found"
        );

        match launch(
            &opening.command,
            false,
            Pads::ForAnApplication,
            &self.wayland_display,
            self.xwayland_display.as_deref(),
        ) {
            Ok(child) => {
                let pid = child.id();
                tracing::info!(command = %opening.command, pid, "player process started");
                self.launched_apps.push(LaunchedApp {
                    name: title.to_string(),
                    command: opening.command,
                    child,
                    started_at: Instant::now(),
                    wait_error_reported: false,
                    played,
                    through_heroic: false,
                });
                Some(pid)
            }
            Err(err) => {
                tracing::warn!(command = %opening.command, ?err, "failed to start a player");
                None
            }
        }
    }

    /// Start a program that is not opening anything, and file it under its own
    /// name.
    ///
    /// The one launch here with no file behind it. Everything else on this bar
    /// is a thing — an application's desktop entry, a song, a game — and is
    /// filed under what that thing is called; this is a program being asked for
    /// as itself, which the shell does in exactly one place: the row that
    /// stands in for RetroArch offers RetroArch's own interface, because the
    /// shell hides it from the list of installed applications and there would
    /// otherwise be no way to reach it.
    ///
    /// It joins the launched applications on the same terms as anything else,
    /// so the guide can close it like anything else.
    pub fn open_command(&mut self, opening: crate::media::Opening) -> Option<u32> {
        match launch(
            &opening.command,
            false,
            Pads::ForAnApplication,
            &self.wayland_display,
            self.xwayland_display.as_deref(),
        ) {
            Ok(child) => {
                let pid = child.id();
                tracing::info!(command = %opening.command, pid, "a program was started as itself");
                self.launched_apps.push(LaunchedApp {
                    name: opening.name,
                    command: opening.command,
                    child,
                    started_at: Instant::now(),
                    wait_error_reported: false,
                    played: None,
                    through_heroic: false,
                });
                Some(pid)
            }
            Err(err) => {
                tracing::warn!(command = %opening.command, ?err, "it would not start");
                None
            }
        }
    }

    /// Name of the most recently started application still running, if any.
    ///
    /// Used to label the guide menu when the compositor does not supply a
    /// foreground title — which is the case on any compositor other than
    /// LineXinBar, where the shell is an ordinary layer-shell client.
    pub fn running_app(&self) -> Option<&str> {
        self.launched_apps.last().map(|app| app.name.as_str())
    }

    /// The installed application a window belongs to, found by the name the
    /// window calls itself by.
    ///
    /// The catalogue is what turns an `app_id` into something a user reads:
    /// `firefox` is a window class, "Firefox" is what the desktop entry that
    /// installed it says the application is called. The whole tree is searched,
    /// subcategories included, because a window says nothing about which column
    /// of the bar its application was filed into.
    ///
    /// Matching is [`App::owns_window`]'s, the same question the bar asks
    /// before starting anything, so a window can never be one application's
    /// when it is being brought back and another's when it is being closed.
    ///
    /// The applications held [`aside`] are searched after the tree. They are
    /// installed and can be running like any other — they simply have no tile —
    /// and a window nobody claims is named by the machine's spelling of its
    /// `app_id`, so leaving them out is how the guide comes to offer "Close
    /// Imagonsole" over a window every other part of the session calls
    /// Pictures.
    ///
    /// [`aside`]: Self::aside
    pub fn app_for_window(&self, app_id: &str) -> Option<&App> {
        fn search<'a>(entries: &'a [Entry], app_id: &str) -> Option<&'a App> {
            entries.iter().find_map(|entry| match entry {
                Entry::App(app) if app.owns_window(app_id) => Some(app),
                _ => search(entry.entries()?, app_id),
            })
        }
        if app_id.trim().is_empty() {
            return None;
        }
        self.categories
            .iter()
            .find_map(|category| search(&category.entries, app_id))
            .or_else(|| self.aside.iter().find(|app| app.owns_window(app_id)))
    }

    /// Whether the process behind `pid` is one of ours and still running.
    ///
    /// True only for as long as [`reap_children`] has not collected it, which
    /// the shell does once around every turn of its loop. What it buys is a
    /// splash that gives up the moment a launch dies instead of waiting out
    /// its patience over an application that will never appear.
    ///
    /// [`reap_children`]: Self::reap_children
    pub fn launch_alive(&self, pid: u32) -> bool {
        self.launched_apps.iter().any(|app| app.child.id() == pid)
    }

    /// Ask everything this shell started to close.
    ///
    /// The fallback for when there is no compositor to ask politely. Children
    /// are their own process group leaders (see [`launch`]), so signalling the
    /// group reaches the real application even when it was started through a
    /// wrapper script.
    pub fn terminate_launched_apps(&mut self) {
        for launched in &self.launched_apps {
            let pid = launched.child.id() as libc::pid_t;
            tracing::info!(app = %launched.name, pid, "asking application to exit");
            // SAFETY: `pid` names a child this process started and has not yet
            // reaped, so the process group cannot have been recycled.
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
        }
    }

    /// Collect exit statuses from launched applications without blocking the
    /// UI. Keeping the direct child makes failures such as `sh -c` exiting 127
    /// visible, while `setsid` and null stdio keep a running app independent of
    /// the shell's terminal and input lifecycle.
    /// Answers the games that came back having failed — see [`Played`]. Every
    /// other kind of exit is tidied away and logged, which is all any of them
    /// has ever needed.
    pub fn running_roms(&self) -> usize {
        self.launched_apps
            .iter()
            .filter(|app| app.played.is_some())
            .count()
    }

    pub fn reap_children(&mut self) -> Vec<Played> {
        let now = Instant::now();
        let mut failed = Vec::new();
        self.launched_apps
            .retain_mut(|launched| match launched.child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt;
                    let lasted = now.duration_since(launched.started_at);
                    let runtime_ms = lasted.as_millis();
                    match (status.success(), status.signal()) {
                        (true, _) => tracing::info!(
                            app = %launched.name,
                            command = %launched.command,
                            %status,
                            runtime_ms,
                            "application process exited"
                        ),
                        // Close ends an application with a signal on purpose,
                        // so this is the ordinary way a launch finishes here —
                        // not a failure, and not something to warn about every
                        // time the user closes something.
                        (false, Some(signal)) => tracing::info!(
                            app = %launched.name,
                            command = %launched.command,
                            signal,
                            runtime_ms,
                            "application process was ended"
                        ),
                        (false, None) => {
                            tracing::warn!(
                                app = %launched.name,
                                command = %launched.command,
                                %status,
                                runtime_ms,
                                "application process exited unsuccessfully"
                            );
                            // A game is the one kind of failure the shell has
                            // something to say about. Reported with what it
                            // was rather than acted on here: this is the
                            // catalogue, and what to put on the screen is the
                            // shell's business.
                            if let Some(played) = launched.played.clone() {
                                failed.push(Played { lasted, ..played });
                            }
                        }
                    }
                    false
                }
                Err(err) => {
                    if !launched.wait_error_reported {
                        tracing::warn!(
                            app = %launched.name,
                            command = %launched.command,
                            pid = launched.child.id(),
                            ?err,
                            "could not query application process status"
                        );
                        launched.wait_error_reported = true;
                    }
                    true
                }
            });
        failed
    }
}

impl Cursor {
    pub fn new(categories: usize) -> Self {
        Self {
            selected_category: 0,
            selected_items: vec![None; categories],
            stack: Vec::new(),
            open: 0,
            category_position: 0.0,
            item_position: 0.0,
            depth_position: 0.0,
            category_speed: 0.0,
            item_speed: 0.0,
            depth_speed: 0.0,
        }
    }

    /// A cursor for a display just coming up, resting on the column the user
    /// asked a session to open on — or, failing that, on the first column that
    /// has anything in it.
    ///
    /// The setting is Settings > System > Startup category and it comes first,
    /// whatever is in the column it names: somebody who chose to open on
    /// Settings meant it, and Settings has nothing launchable in it by design.
    ///
    /// The fallback is what the shell did before there was a setting, and it is
    /// still needed for two reasons. A column can be named by the setting and
    /// not be on this bar — a Steam library nobody has signed in to, a Waydroid
    /// that is not installed — and a shell that insisted would open on whatever
    /// happened to have taken that column's index. And the shell's own Settings
    /// column leads the bar with nothing under it, so a session that simply
    /// took the first column would open onto an empty one, which is a poor
    /// greeting.
    ///
    /// Either way it is placed rather than travelled to, so the bar is already
    /// where it belongs on the first frame.
    pub fn for_model(lattice: &Lattice) -> Self {
        let mut cursor = Self::new(lattice.categories.len());
        let wanted = crate::settings::startup_category();
        let opening = lattice
            .categories
            .iter()
            .position(|column| column.id == wanted)
            .or_else(|| lattice.categories.iter().position(Category::has_launchable));
        if let Some(populated) = opening {
            cursor.selected_category = populated;
            cursor.category_position = populated as f32;
        }
        // On the head of that column's list, by the same rule every other
        // arrival lands under — a session that opened on the row above a
        // library would open on its index.
        cursor.restore_column(lattice);
        cursor
    }

    /// The row the cursor is on, in whichever column it is standing in.
    pub fn selected_item(&self) -> usize {
        match self.open.checked_sub(1) {
            None => self.row_at(0),
            Some(level) => self.stack.get(level).map_or(0, |column| column.selected),
        }
    }

    /// How many subcategories deep the cursor is standing.
    ///
    /// Drawing asks where the bar *is* — [`Self::depth_position`] — while
    /// input routing asks how many steps opened the path: a move whose depth
    /// falls is the one that needs the distinct back sound.
    pub fn depth(&self) -> usize {
        self.open
    }

    /// The same, eased — where the bar is *drawn*, which is somewhere between
    /// two depths for as long as a step in or out is still travelling.
    pub fn depth_position(&self) -> f32 {
        self.depth_position
    }

    pub fn current_category<'a>(&self, lattice: &'a Lattice) -> Option<&'a Category> {
        lattice.categories.get(self.selected_category)
    }

    /// The row the column the cursor is standing in was opened from — the last
    /// step of the trail, one level up.
    ///
    /// `None` at the top of a category, where the column is the category's own
    /// and was not opened from anything. What it answers is "what is this a
    /// list *of*", which is how a panel raised inside a column is titled.
    pub fn open_from<'a>(&self, lattice: &'a Lattice) -> Option<&'a Entry> {
        let level = self.open.checked_sub(1)?;
        self.level_entries(lattice, level)?.get(self.row_at(level))
    }

    /// Every row the cursor has stepped *into*, outermost first.
    ///
    /// Opened rather than standing on, which is the difference this answers: a
    /// cursor resting on a row has not opened it, and the last column — the one
    /// the cursor is actually in — was opened from the row before it. So the
    /// trail is levels `0..open` and stops there.
    ///
    /// What wants to know is a page whose being open costs the machine
    /// something — see [`crate::bluetooth::Bt::watch`], where the answer decides
    /// whether a radio is told to look around, and where being one column out is
    /// the difference between scanning because somebody asked and scanning
    /// because somebody walked past.
    pub fn opened_rows<'a>(&self, lattice: &'a Lattice) -> Vec<&'a Entry> {
        (0..self.open)
            .filter_map(|level| self.level_entries(lattice, level)?.get(self.row_at(level)))
            .collect()
    }

    /// The rows of the column the cursor is standing in.
    pub fn current_entries<'a>(&self, lattice: &'a Lattice) -> &'a [Entry] {
        self.level_entries(lattice, self.open).unwrap_or_default()
    }

    /// The row the cursor is on.
    pub fn current_entry<'a>(&self, lattice: &'a Lattice) -> Option<&'a Entry> {
        self.current_entries(lattice).get(self.selected_item())
    }

    /// What the cursor is on, when it is on something launchable. `None` for a
    /// subcategory or a setting, neither of which starts a process.
    pub fn current_app<'a>(&self, lattice: &'a Lattice) -> Option<&'a App> {
        self.current_entry(lattice)?.app()
    }

    /// What the highlighted value would apply, without moving the catalogue's
    /// chosen mark. The shell uses this to preview a setting while the cursor
    /// is merely standing on it.
    pub fn current_setting(&self, lattice: &Lattice) -> Option<Setting> {
        self.current_entry(lattice)?.setting()
    }

    /// Every column the bar is showing, outermost first: the category's own,
    /// then one per subcategory stepped into — and, for as long as it is still
    /// sliding away, the one just stepped out of.
    pub fn columns<'a>(&self, lattice: &'a Lattice) -> Vec<Column<'a>> {
        let mut out = Vec::with_capacity(self.stack.len() + 1);
        for level in 0..=self.stack.len() {
            let Some(entries) = self.level_entries(lattice, level) else {
                break;
            };
            out.push(Column {
                entries,
                // Clamped rather than trusted: the catalogue is not the
                // cursor's to keep in step with, and a row that has gone out
                // from under it must not take the drawing with it.
                selected: self.row_at(level).min(entries.len().saturating_sub(1)),
                position: self.position_at(level),
                // Deeper than the user is standing: the only column that can
                // be is the one just stepped out of, which `stack` holds on to
                // for exactly as long as it takes to leave.
                standing: match level.cmp(&self.open) {
                    std::cmp::Ordering::Less => Standing::Behind,
                    std::cmp::Ordering::Equal => Standing::Open,
                    std::cmp::Ordering::Greater => Standing::Leaving,
                },
            });
        }
        out
    }

    /// The rows of the column at `level`, 0 being the category's own. `None`
    /// once the path stops leading anywhere, which is how a stale stack — one
    /// left over from a catalogue that has since changed — stops short instead
    /// of drawing something that is no longer there.
    fn level_entries<'a>(&self, lattice: &'a Lattice, level: usize) -> Option<&'a [Entry]> {
        let mut entries = lattice
            .categories
            .get(self.selected_category)?
            .entries
            .as_slice();
        for step in 0..level {
            entries = entries.get(self.row_at(step))?.entries()?;
        }
        Some(entries)
    }

    /// The same column, to be changed. Written out again rather than shared
    /// with [`Self::level_entries`]: one returns a shared borrow of the
    /// catalogue and the other an exclusive one, and there is no way to have
    /// the second call the first.
    fn level_entries_mut<'a>(
        &self,
        lattice: &'a mut Lattice,
        level: usize,
    ) -> Option<&'a mut [Entry]> {
        let mut entries = lattice
            .categories
            .get_mut(self.selected_category)?
            .entries
            .as_mut_slice();
        for step in 0..level {
            entries = entries.get_mut(self.row_at(step))?.entries_mut()?;
        }
        Some(entries)
    }

    /// The rows of the column the cursor is standing in, as the list they
    /// really are — so a row can be put on it or taken off rather than only
    /// changed.
    ///
    /// The one caller is the head row a marking stands at the top of a column
    /// in place of the column's own; see [`crate::marks`]. Everything else in
    /// this shell builds a column whole and replaces it whole, which is why
    /// [`Self::level_entries_mut`] hands back a slice and this is separate
    /// rather than the two being one call.
    pub fn open_column_mut<'a>(&self, lattice: &'a mut Lattice) -> Option<&'a mut Vec<Entry>> {
        let mut entries = &mut lattice.categories.get_mut(self.selected_category)?.entries;
        for step in 0..self.open {
            entries = entries.get_mut(self.row_at(step))?.entries_vec_mut()?;
        }
        Some(entries)
    }

    /// Make the row under the cursor the one its column is set to, and say
    /// what that means — which is for the caller to put into force.
    ///
    /// `None`, and nothing moved, unless the cursor is on a value that stands
    /// for a setting: an application, a subcategory, and a value the shell can
    /// only display are all rows that this leaves exactly as they were.
    ///
    /// The mark is exclusive within its column and only within it. A column of
    /// values is one answer to one question, so choosing an answer un-chooses
    /// the others; a list somewhere else in the tree is a different question
    /// and is none of this one's business.
    pub fn choose(&self, lattice: &mut Lattice) -> Option<Setting> {
        let row = self.selected_item();
        let entries = self.level_entries_mut(lattice, self.open)?;
        let (setting, acts) = match entries.get(row)? {
            Entry::Choice(choice) => (choice.setting?, choice.acts),
            _ => return None,
        };
        // A row that acts is not one of the answers this column holds, so the
        // marks are none of its business — see [`crate::apps::Choice::acts`].
        // Forgetting a network leaves the column saying exactly what it said;
        // moving the mark here would put a tick on a press and take it off the
        // value that is still in force.
        if acts {
            return Some(setting);
        }
        for (index, entry) in entries.iter_mut().enumerate() {
            match entry {
                Entry::Choice(choice) => choice.chosen = index == row,
                // A subcategory can carry the mark too — see
                // [`crate::apps::Folder::chosen`] — and it is cleared here for
                // the same reason every other row's is. The tree these live in
                // is rebuilt from what a worker reports a moment later and will
                // say so again if it is still true; what must not happen in
                // between is a column showing two answers to one question.
                Entry::Folder(folder) => folder.chosen = false,
                _ => {}
            }
        }
        Some(setting)
    }

    /// Read the folder under the cursor off the disk, so that stepping into it
    /// steps into what is there *now*.
    ///
    /// The file explorer's columns are the only ones in the tree that are not
    /// known in advance, and this is where they come from. It runs on the press
    /// that opens a folder — Accept, or Right from inside a column — and never
    /// on the way past: a cursor walking down `/usr` passes a hundred
    /// directories on its way to one of them, and reading each as it went by
    /// would be a hundred directories read to answer nothing.
    ///
    /// Read every time rather than kept, which is what makes the column honest
    /// about a file that has been added or deleted since the user was last in
    /// it. It also bounds what the tree holds: the *other* folders of the same
    /// column give up their rows here, so what is left in memory is the path
    /// the user is standing in and not everywhere they have been.
    ///
    /// The remembered row of the column about to be opened goes with them, for
    /// the same reason — it was a row number in a listing that no longer
    /// exists, and a folder read afresh opens at the top of itself.
    ///
    /// Returns whether anything was read, which is `false` for every row in the
    /// shell that is not one of the explorer's.
    pub fn open_place(
        &mut self,
        lattice: &mut Lattice,
        how: crate::files::How,
    ) -> Option<crate::media::Orders> {
        let row = self.selected_item();
        let entries = self.level_entries_mut(lattice, self.open)?;
        // A fresh visit, so the field it opens with is empty. A search belongs
        // to the looking somebody is doing rather than to the folder — walking
        // out of one and into another is a different question, and a column
        // that arrived already narrowed by what was typed in the last one would
        // be hiding files with no field in sight to say so.
        let orders = read_into(entries, row, "", how)?;

        for (index, entry) in entries.iter_mut().enumerate() {
            if index == row {
                continue;
            }
            if let Entry::Folder(folder) = entry {
                if folder.place.is_some() {
                    folder.entries = Vec::new();
                }
            }
        }
        self.stack.truncate(self.open);
        Some(orders)
    }

    /// Walk into the file explorer, down to `folder`, and leave `item` under
    /// the cursor.
    ///
    /// What "Show in folder" comes down to; see [`crate::reveal`], which is
    /// where the question arrives from. `None` when there is no way there on
    /// this bar — no Files row, a path on no disk the shell lists, a folder
    /// that cannot be read — and otherwise what the column it ended in could
    /// be ordered by, which is [`Self::open_place`]'s own answer for the last
    /// read.
    ///
    /// The route is [`crate::picker::Picker::walk_to`]'s, made against the
    /// bar's tree instead of a panel's columns: the disks are opened, the one
    /// holding the path is chosen — the deepest of them, so a file in the
    /// user's own folder opens under Home rather than four columns down from
    /// Root — and then one column per part of what is left.
    ///
    /// Every one of those columns is read off the disk as it is stepped into,
    /// exactly as a press reads one. That is one `readdir` per level of the
    /// path, on the thread that draws — a folder is under a millisecond, and a
    /// download is four levels down. See [`Self::open_place`] for what each of
    /// those reads throws away.
    ///
    /// Arriving is standing in the folder. A file that is not among its rows —
    /// a listing capped at [`crate::files`]'s ten-thousandth row, or something
    /// deleted between the question and this frame — is still an arrival: the
    /// folder is what the user is shown, which is most of what was asked for,
    /// and a walk that reported failure would have them shown nothing at all.
    ///
    /// A walk that does *not* arrive leaves the cursor wherever it got to,
    /// which is [`crate::Shell::walk_to_the_bios_row`]'s behaviour and is
    /// deliberate on the same grounds: putting it back would mean restoring a
    /// position into a tree this walk has already rewritten underneath it —
    /// every column it read dropped the rows of the folders beside it. Nothing
    /// is *shown* moving either way, because the caller only brings the bar
    /// forward once this has answered.
    pub fn walk_to_file(
        &mut self,
        lattice: &mut Lattice,
        folder: &Path,
        item: Option<&Path>,
        how: crate::files::How,
    ) -> Option<crate::media::Orders> {
        let (category, row) = the_files_row(&lattice.categories)?;
        // The column itself, and not whichever subcolumn of it somebody
        // happened to be standing in — see [`Self::go_to_own_column`], which
        // exists for this.
        self.go_to_own_column(category, lattice);
        self.point_at_row(row, lattice);
        // The disks, which are worked out on the press that opens Files rather
        // than kept: a drive plugged in an hour into the session has to be
        // among them for a file on it to be reachable at all.
        let mut orders = self.open_and_enter(lattice, how)?;

        let root = deepest_disk_holding(self.current_entries(lattice), folder)?;
        let rest = folder.strip_prefix(&root).ok()?;
        let mut trail = vec![root.clone()];
        let mut walked = root;
        for part in rest.components() {
            walked = walked.join(part);
            trail.push(walked.clone());
        }
        for step in &trail {
            let row = row_leading_to(self.current_entries(lattice), step)?;
            self.point_at_row(row, lattice);
            orders = self.open_and_enter(lattice, how)?;
        }

        if let Some(row) = item.and_then(|item| row_leading_to(self.current_entries(lattice), item))
        {
            self.point_at_row(row, lattice);
        }
        Some(orders)
    }

    /// Read the folder under the cursor off the disk and step into it.
    ///
    /// The two halves of opening one of the explorer's columns, in the order
    /// every press does them in: what is on a disk is not known until it has
    /// been looked at, and a column with no rows cannot be stepped into.
    fn open_and_enter(
        &mut self,
        lattice: &mut Lattice,
        how: crate::files::How,
    ) -> Option<crate::media::Orders> {
        let orders = self.open_place(lattice, how)?;
        self.enter(lattice).then_some(orders)
    }

    /// Read the folder the cursor is standing *in* again, keeping only what
    /// answers to `query`.
    ///
    /// The other end of the field at the head of an explorer column. A shelf is
    /// narrowed by asking the worker that holds half a million files; a folder
    /// is narrowed by looking at it again, because looking at it is what a
    /// folder costs — one `readdir`, and a `stat` for each row the query keeps.
    /// Which means a search of a large directory is *cheaper* than opening it.
    ///
    /// The folder is the row the column was opened from, one level up, so this
    /// answers `false` at the top of a category where there is no such row.
    pub fn search_here(
        &self,
        lattice: &mut Lattice,
        query: &str,
        how: crate::files::How,
    ) -> Option<crate::media::Orders> {
        let level = self.open.checked_sub(1)?;
        let row = self.row_at(level);
        let entries = self.level_entries_mut(lattice, level)?;
        read_into(entries, row, query, how)
    }

    /// The row selected in the column at `level`.
    fn row_at(&self, level: usize) -> usize {
        match level.checked_sub(1) {
            // The head of the column for one that has never been stood in:
            // the row a cursor arrives on is the top of the list. See
            // [`Self::row_in_column`], which is the same question asked of a
            // column the cursor is *not* in and answers `None` there.
            None => self
                .selected_items
                .get(self.selected_category)
                .copied()
                .flatten()
                .unwrap_or(0),
            Some(index) => self.stack.get(index).map_or(0, |column| column.selected),
        }
    }

    fn position_at(&self, level: usize) -> f32 {
        match level.checked_sub(1) {
            None => self.item_position,
            Some(index) => self.stack.get(index).map_or(0.0, |column| column.position),
        }
    }

    /// Move the cursor to `row` in the column it is standing in.
    fn select_row(&mut self, row: usize) {
        match self.open.checked_sub(1) {
            None => {
                if let Some(slot) = self.selected_items.get_mut(self.selected_category) {
                    *slot = Some(row);
                }
            }
            Some(level) => {
                if let Some(column) = self.stack.get_mut(level) {
                    column.selected = row;
                }
            }
        }
        // Whatever was kept beyond here hung off the row being left, so it is
        // no longer a column anybody can step back into.
        self.stack.truncate(self.open);
    }

    /// Bring the cursor back inside the columns it is standing in, after the
    /// tree has been rebuilt underneath it.
    ///
    /// The pages that describe hardware are written from what a worker last
    /// reported and rewritten whenever that changes, so a column can lose rows
    /// while somebody is standing on one of them: a cable is pulled out, a
    /// network goes out of range, and — the one that is not an accident — a
    /// value is cleared and the row that held it is no longer part of the
    /// question. `Settings > Network > DNS` is where that last one happens.
    /// Emptying the list of servers hands the question back to the network, and
    /// the row the field was opened from stops existing on the same frame.
    ///
    /// A cursor left pointing past the end of a column is not a cursor on the
    /// last row: it is a column drawn scrolled off its own bottom, with nothing
    /// under the highlight and no direction that gets back to the list except
    /// Up, pressed as many times as the rows that vanished. So every column on
    /// the path is checked, not only the open one — the trail behind it is on
    /// screen too, and a subcategory that fell off the end of the column it
    /// hangs from would take the path with it.
    ///
    /// Where the row is gone, the cursor goes to *the value in force* if the
    /// column has one, and to its last row otherwise. That is the same rule a
    /// column is opened under — see [`Self::open_entry`] — and it is the right
    /// one here for the same reason: what is left of a question whose answer
    /// has just been withdrawn is the answer that is now true.
    ///
    /// A column that has emptied altogether is stepped out of. There is no row
    /// to put the cursor on, and a column with nothing in it is the one shape
    /// the bar cannot show.
    ///
    /// Reports whether anything moved, which is what says a redraw is owed.
    pub fn keep_in_bounds(&mut self, lattice: &Lattice) -> bool {
        let mut moved = false;
        // Outermost first, because a level's own column is reached through the
        // rows above it: clamping level 2 is only meaningful once level 1 is
        // pointing at a row that exists.
        for level in 0..=self.open {
            // No column here at all — the row this one hung from is not a
            // subcategory any more — or one with nothing in it. The two are the
            // same thing to a cursor: everything from here down goes, and it
            // stands in the last column that is still real.
            let entries = self.level_entries(lattice, level).unwrap_or_default();
            if entries.is_empty() {
                let out = level.saturating_sub(1);
                // Nothing has moved where the cursor was already outside: a
                // category with no rows in it is a category the bar is standing
                // *in front of*, not one it has to be got out of, and saying a
                // redraw is owed for it would owe one on every rebuild.
                moved |= out != self.open || self.stack.len() > out;
                self.open = out;
                self.stack.truncate(out);
                break;
            }
            let row = self.row_at(level);
            if row < entries.len() {
                continue;
            }
            let landing = Self::landing(entries);
            self.set_row_at(level, landing);
            moved = true;
        }
        moved
    }

    /// Which row a cursor put back inside a column should come to rest on.
    ///
    /// The value in force, which is where a column is opened anyway: somebody
    /// whose row went out from under them is looking at the same question, and
    /// the answer to it is the least surprising place to be standing.
    ///
    /// Failing that, the last row — a column that lost rows off its end has
    /// most naturally clamped to the end — but never a row that *acts*. That is
    /// the one hard rule here, and it is worth the extra line: pressing
    /// Disconnect under a wireless network turns that page into a shorter one,
    /// and a cursor that clamped to the end of it would leave the user's thumb
    /// resting on Forget with no press of their own in between. The shell moved
    /// the cursor; the shell does not get to move it somewhere that deletes
    /// something.
    ///
    /// The first row where every row acts, for the same reason the rest of this
    /// tree puts the answer that does least at the top.
    fn landing(entries: &[Entry]) -> usize {
        if let Some(chosen) = entries.iter().position(Entry::chosen) {
            return chosen;
        }
        entries
            .iter()
            .rposition(|entry| !entry.acts())
            .unwrap_or_default()
    }

    /// Put the cursor on `row` of the column at `level`, wherever that column
    /// stands on the path.
    ///
    /// Unlike [`Self::select_row`] this keeps what hangs below: it is used
    /// where a column has been rewritten under a cursor that has not moved, and
    /// the trail the user opened is still theirs.
    fn set_row_at(&mut self, level: usize, row: usize) {
        match level.checked_sub(1) {
            None => {
                if let Some(slot) = self.selected_items.get_mut(self.selected_category) {
                    *slot = Some(row);
                }
            }
            Some(index) => {
                if let Some(column) = self.stack.get_mut(index) {
                    column.selected = row;
                }
            }
        }
    }

    /// Put the cursor straight on `row` of the column it is standing in.
    ///
    /// What a mouse or a finger does, and the one way of moving the cursor that
    /// is not a step: a pointer names the row it wants outright, where a
    /// direction can only ask for the next one. The bar still *travels* there —
    /// the eased position is untouched — so a click three rows down looks like
    /// the column being scrolled to rather than the list being replaced.
    ///
    /// Reports whether that moved anything, and refuses a row the column does
    /// not have.
    pub fn point_at_row(&mut self, row: usize, lattice: &Lattice) -> bool {
        if row >= self.current_entries(lattice).len() || row == self.selected_item() {
            return false;
        }
        self.select_row(row);
        true
    }

    /// The same for the category row: put the cursor on category `index`.
    ///
    /// Steps out of whatever path is open first, exactly as walking left to the
    /// row would: the categories are behind the columns, and arriving at one
    /// with a path still open would leave the cross showing a trail belonging
    /// to a category the user is no longer in.
    pub fn point_at_category(&mut self, index: usize, lattice: &Lattice) -> bool {
        if index >= lattice.categories.len() {
            return false;
        }
        if index == self.selected_category && self.open == 0 {
            return false;
        }
        self.selected_category = index;
        self.restore_column(lattice);
        true
    }

    /// Keep the cursor on the file it is standing on, after rows have been put
    /// into the column above or below it.
    ///
    /// Music arrives all through a session and arrives *in order*, so a track
    /// found now goes wherever the alphabet says rather than onto the end.
    /// Without this the row under the cursor would change identity every time
    /// the walk turned up something earlier in the alphabet — the user would
    /// be looking at one song and pressing another.
    ///
    /// The drawn position moves with it, by the same distance. What happened is
    /// that the list slid under a stationary cursor, and a spring left to
    /// travel would instead scroll the column past the row being read.
    ///
    /// The file is named by the handle the row held rather than by its path.
    /// Every list is built out of the same shared files, so this is a pointer
    /// against a pointer where a path would be a string against a string — and
    /// it is asked of every row of a shelf that may hold half a million. A file
    /// that has been found *again* since — deleted and put back between two
    /// passes of the walk — is a different handle, and the cursor treats it as
    /// the file having gone, which is what it did.
    pub fn keep_on_media(&mut self, lattice: &Lattice, file: &crate::media::Shelved) {
        let was = self.selected_item();
        let Some(row) = self
            .current_entries(lattice)
            .iter()
            .position(|entry| entry.shelved().is_some_and(|held| Arc::ptr_eq(held, file)))
        else {
            // The file has gone off the disk. The cursor keeps its row, which
            // is now whichever file closed the gap — the same thing that
            // happens to a paper list when a line is struck out of it.
            return;
        };
        if row == was {
            return;
        }
        self.select_row(row);
        self.shift_position(row as f32 - was as f32);
    }

    /// The row this cursor has of its own in the column at `at`, if it has one.
    ///
    /// Two ways to have one, and they are the two halves of what the bar means
    /// by "where this display is": the cursor is standing in the column, or it
    /// has stood in it and left a row behind. A column it has never walked into
    /// has neither. Its slot holds the row a *first* visit would begin at,
    /// which is the head of the list — a starting point, not a place the
    /// display is.
    ///
    /// The difference only shows itself where the column is rebuilt underneath
    /// a cursor that is elsewhere, which is exactly what a Steam library does
    /// while the shell is starting: the installed games arrive off the disk
    /// first and the rest of the account's library a moment later. Read as a
    /// row, the head of that half-built list would be a game this display was
    /// standing on, and it would be followed to wherever the finished order
    /// puts it — leaving somebody who has never opened Steam to walk in on the
    /// middle of their library rather than the top of the order they chose.
    fn row_in_column(&self, at: usize) -> Option<usize> {
        match *self.selected_items.get(at)? {
            Some(row) => Some(row),
            // Standing in it without having moved down it. The head of the
            // column is under the highlight and is being looked at, which is
            // the whole of what a kept row is for.
            None => (self.selected_category == at).then_some(0),
        }
    }

    /// Which game this cursor's row in the column at `at` is on, if it is on
    /// one. The pair of [`Self::keep_on_game`], asked before the column is
    /// rebuilt so there is something to keep it on afterwards.
    ///
    /// The column at `at` rather than the one the cursor is standing in: the
    /// row a cursor is on in a category it is *not* in is remembered all the
    /// same, and walking back to a library that re-sorted while the user was
    /// elsewhere would land them on a different game for exactly the same
    /// reason. A column it has *never* been in has no such row — see
    /// [`Self::row_in_column`] — and answers `None` however many games are
    /// hanging in it.
    pub fn game_in_column(&self, lattice: &Lattice, at: usize) -> Option<u32> {
        let row = self.row_in_column(at)?;
        Some(lattice.categories.get(at)?.entries.get(row)?.game()?.app_id)
    }

    /// The same question of the Epic Games column, whose games are known by
    /// Epic's app name rather than by a number.
    pub fn epic_game_in_column(&self, lattice: &Lattice, at: usize) -> Option<String> {
        let row = self.row_in_column(at)?;
        let entry = lattice.categories.get(at)?.entries.get(row)?;
        Some(entry.epic_game()?.app_name.clone())
    }

    /// Remember the full path, including Alphabetical and its letter folders.
    pub fn trophy_selection(&self, lattice: &Lattice) -> Option<Vec<crate::trophies::Position>> {
        let at = lattice
            .categories
            .iter()
            .position(|c| c.id == crate::trophies::COLUMN)?;
        let mut entries = lattice.categories[at].entries.as_slice();
        let mut row = self.row_in_column(at)?;
        let depth = if self.selected_category == at {
            self.stack.len()
        } else {
            0
        };
        let mut path = Vec::new();
        for level in 0..=depth {
            let Some(entry) = entries.get(row) else {
                break;
            };
            let Some(key) = crate::trophies::Position::of(entry) else {
                break;
            };
            path.push(key);
            let Some(children) = entry.entries() else {
                break;
            };
            entries = children;
            row = self.row_at(level + 1);
        }
        (!path.is_empty()).then_some(path)
    }

    pub fn keep_on_trophy(&mut self, lattice: &Lattice, path: &[crate::trophies::Position]) {
        let Some(at) = lattice
            .categories
            .iter()
            .position(|c| c.id == crate::trophies::COLUMN)
        else {
            return;
        };
        let mut entries = lattice.categories[at].entries.as_slice();
        for (level, key) in path.iter().enumerate() {
            let found = entries
                .iter()
                .position(|entry| crate::trophies::Position::of(entry).as_ref() == Some(key));
            let row = found.unwrap_or_else(|| first_row(entries));
            if self.selected_category != at {
                self.selected_items[at] = Some(row);
                break;
            }
            let was = self.row_at(level);
            self.set_row_at(level, row);
            if level == 0 {
                self.item_position += row as f32 - was as f32;
            } else if let Some(column) = self.stack.get_mut(level - 1) {
                column.position += row as f32 - was as f32;
            }
            if found.is_none() {
                // A filtered-out game or letter cannot keep its descendants open.
                self.open = self.open.min(level);
                self.stack.truncate(level);
                break;
            }
            let Some(children) = entries.get(row).and_then(Entry::entries) else {
                break;
            };
            entries = children;
        }
    }

    /// Keep the cursor on the game it was on, after the column at `at` has been
    /// re-sorted under it.
    ///
    /// A game finishing its download moves from the half of the library that
    /// cannot be played to the half that can, which is halfway up a list of
    /// hundreds. Somebody watching that download finish is watching *that* row,
    /// and a cursor that stayed on the row number would leave them looking at
    /// whichever title closed the gap — with no way of knowing where the game
    /// they were waiting for has gone.
    ///
    /// Nothing moves on screen. The column is redrawn from a row further up,
    /// and the drawn position is moved by the same distance, so the same cover
    /// is under the same highlight on the frame after as on the frame before —
    /// the list has been re-sorted, which is not a journey the user made. This
    /// is [`Self::keep_on_media`]'s rule, applied to a list that re-sorts
    /// itself rather than one that grows.
    pub fn keep_on_game(&mut self, lattice: &Lattice, at: usize, app_id: u32) {
        self.keep_on_row(lattice, at, |entry| {
            entry.game().is_some_and(|game| game.app_id == app_id)
        });
    }

    /// [`Self::keep_on_game`] for the Epic Games column.
    pub fn keep_on_epic_game(&mut self, lattice: &Lattice, at: usize, app_name: &str) {
        self.keep_on_row(lattice, at, |entry| {
            entry
                .epic_game()
                .is_some_and(|game| game.app_name == app_name)
        });
    }

    /// Keep the cursor on the row `is` picks out, wherever the column moved it.
    fn keep_on_row(&mut self, lattice: &Lattice, at: usize, is: impl Fn(&Entry) -> bool) {
        let Some(was) = self.row_in_column(at) else {
            return;
        };
        let Some(entries) = lattice.categories.get(at).map(|column| &column.entries) else {
            return;
        };
        let Some(row) = entries.iter().position(is) else {
            // The game has left the library altogether — a shared title whose
            // lender took it back. The cursor keeps its row, which is now
            // whichever game closed the gap, exactly as [`Self::keep_on_media`]
            // leaves a deleted file's row alone.
            return;
        };
        if row == was {
            return;
        }
        self.selected_items[at] = Some(row);
        // Only the column that is on screen has a drawn position to move, and
        // for a category it is always the outermost one: a game is the end of a
        // path, so there is never a subcolumn open over the top of this.
        if self.selected_category == at {
            self.item_position += row as f32 - was as f32;
        }
    }

    /// The game the cursor is standing on *inside* a column it stepped into, if
    /// it is in one and on one.
    ///
    /// The depth [`Self::game_in_column`] does not reach. That one asks what a
    /// cursor is on in a category's own column; a library hangs whole columns of
    /// games off the index at the head of it, and a cursor standing in one of
    /// those is standing on a game the category's column knows nothing about.
    ///
    /// No need to ask which column it is: a game row exists nowhere in this
    /// tree but a Steam library and the letters of its index, so an answer here
    /// is by itself the news that this cursor is inside one.
    pub fn game_inside(&self, lattice: &Lattice) -> Option<u32> {
        if self.open == 0 {
            return None;
        }
        Some(self.current_entry(lattice)?.game()?.app_id)
    }

    /// Keep it on that game after the column it is standing in was rebuilt.
    ///
    /// A letter of the index is installed-first, like the library it is a slice
    /// of, so a download finishing moves its game to the top of the letter —
    /// under the eyes of the one person guaranteed to be watching that row. The
    /// rule and the arithmetic are [`Self::keep_on_media`]'s: the list slid
    /// under a stationary cursor, which is not a journey to show.
    pub fn keep_inside_on_game(&mut self, lattice: &Lattice, app_id: u32) {
        self.keep_inside_on(lattice, |entry| {
            entry.game().is_some_and(|game| game.app_id == app_id)
        });
    }

    /// The Epic game a display standing inside a folder of the Epic Games
    /// column — a letter of its index — is on, for keeping it there across a
    /// rebuild. See [`Self::game_inside`].
    pub fn epic_game_inside(&self, lattice: &Lattice) -> Option<String> {
        if self.open == 0 {
            return None;
        }
        Some(self.current_entry(lattice)?.epic_game()?.app_name.clone())
    }

    /// Keep it on that Epic game after the column was rebuilt. See
    /// [`Self::keep_inside_on_game`].
    pub fn keep_inside_on_epic_game(&mut self, lattice: &Lattice, app_name: &str) {
        self.keep_inside_on(lattice, |entry| {
            entry
                .epic_game()
                .is_some_and(|game| game.app_name == app_name)
        });
    }

    /// Keep a display standing inside a folder on the row `is` picks out.
    fn keep_inside_on(&mut self, lattice: &Lattice, is: impl Fn(&Entry) -> bool) {
        if self.open == 0 {
            return;
        }
        let was = self.selected_item();
        let Some(row) = self.current_entries(lattice).iter().position(is) else {
            // Gone from this letter altogether — a game the account lost, or
            // one renamed into another heading. The cursor keeps its row, as it
            // does for a file deleted from under it.
            return;
        };
        if row == was {
            return;
        }
        self.select_row(row);
        self.shift_position(row as f32 - was as f32);
    }

    /// Put the cursor on the head of the column it is standing in, with the
    /// column drawn there rather than travelling to it.
    ///
    /// For a column that has just been *reordered*: every row in it is a
    /// different row now, so there is no journey through the list to show. A
    /// spring let loose from row twelve thousand would scroll a collection past
    /// the user at a speed nothing could be read at, to arrive somewhere the
    /// list they were looking at no longer exists. Placed, the way a category
    /// being returned to is placed — see [`Self::restore_column`].
    ///
    /// The head of the *list*, not of the column: somebody who has just asked
    /// for "newest first" is asking to be shown the newest, and the search
    /// standing over the shelf is not it.
    pub fn rest_on_first_row(&mut self, lattice: &Lattice) {
        let row = first_row(self.current_entries(lattice));
        self.select_row(row);
        let at = row as f32;
        match self.open.checked_sub(1) {
            None => {
                self.item_position = at;
                self.item_speed = 0.0;
            }
            Some(level) => {
                if let Some(column) = self.stack.get_mut(level) {
                    column.position = at;
                    column.speed = 0.0;
                }
            }
        }
    }

    /// Move where a column is *drawn* without moving what is selected in it.
    fn shift_position(&mut self, rows: f32) {
        match self.open.checked_sub(1) {
            None => self.item_position += rows,
            Some(level) => {
                if let Some(column) = self.stack.get_mut(level) {
                    column.position += rows;
                }
            }
        }
    }

    /// A column has appeared in the bar at `at`. Keep this cursor on the column
    /// it was on.
    ///
    /// Multimedia can arrive mid-session — on a machine with music but no media
    /// player installed there was nothing to make a column out of until the
    /// walk found a file — and every column after it has just moved along one.
    /// The drawn position moves with the selection for the same reason it does
    /// in [`Self::keep_on_media`]: the bar grew, the user did not travel.
    pub fn category_added(&mut self, at: usize) {
        if at > self.selected_items.len() {
            return;
        }
        self.selected_items.insert(at, None);
        if self.selected_category >= at {
            self.selected_category += 1;
            self.category_position += 1.0;
        }
    }

    /// A column has gone from the bar at `at`. Keep this cursor somewhere that
    /// still exists.
    ///
    /// The counterpart of [`Self::category_added`], and it happens for one
    /// reason: signing out of Steam takes the column that account's library
    /// was in off the bar, and a display may well be standing in it. Landing
    /// on the column that closed the gap is what a paper list does when a line
    /// is struck out of it — and it is the same answer this shell gives when a
    /// file the cursor was on is deleted.
    ///
    /// The bar is *placed* rather than travelled, because there is no journey
    /// to show: the column the cursor was in is not somewhere it could travel
    /// from any more.
    pub fn category_removed(&mut self, at: usize, lattice: &Lattice) {
        if at >= self.selected_items.len() {
            return;
        }
        self.selected_items.remove(at);

        let last = lattice.categories.len().saturating_sub(1);
        let was = self.selected_category;
        self.selected_category = match was.cmp(&at) {
            // In front of it: nothing moved.
            std::cmp::Ordering::Less => was,
            // Standing in it, or after it: one column nearer the front.
            _ => was.saturating_sub(1).min(last),
        };
        self.selected_category = self.selected_category.min(last);
        self.category_position = self.selected_category as f32;
        self.category_speed = 0.0;
        // Whatever column that turned out to be, the cursor is at the top of
        // it rather than at a row number carried over from a column that has
        // gone.
        if was >= at {
            self.leave_subcolumns();
            self.rest_on_first_row(lattice);
        }
    }

    /// Where this cursor is standing, said in terms nothing can renumber: the
    /// names of the columns rather than their places on the bar.
    ///
    /// Taken before the bar is rebuilt and handed back to [`Self::recall`]
    /// afterwards. Everything a cursor holds about the bar is an index — which
    /// column it is in, which row it left behind in each of the others — and a
    /// rebuild is a new list of columns those numbers were never about.
    ///
    /// Why a record rather than the list of names the rebuild started with: a
    /// bar is not rebuilt in one move. The scan finds what is on the disk, and
    /// then the Steam library, the trophies and the RetroArch column are hung
    /// on it one at a time — each of them a column *added* under cursors that
    /// are still holding the old bar's numbers, and each of them therefore
    /// pushing those numbers one further along. By the time the cursors were
    /// put back they were indexing the list of old names with a number that had
    /// been walked two or three columns past its own: somebody standing in
    /// Settings when something was installed came back in whatever column
    /// happened to be two along, or — where the number had run off the end —
    /// at the top of the last column on the bar. This is taken before any of
    /// that can touch it.
    pub fn remember(&self, lattice: &Lattice) -> Footing {
        Footing {
            standing: lattice
                .categories
                .get(self.selected_category)
                .map(|column| column.id),
            was: self.selected_category,
            remembered: lattice
                .categories
                .iter()
                .map(|column| column.id)
                .zip(self.selected_items.iter().copied())
                .collect(),
            // How far the bar still had to travel, kept as a distance from the
            // column it was travelling to rather than as a place on a list that
            // is about to be a different length.
            drift: self.category_position - self.selected_category as f32,
            speed: self.category_speed,
            stack: self.stack.clone(),
            open: self.open,
            walked: (0..self.open)
                .map_while(|level| {
                    match self.level_entries(lattice, level)?.get(self.row_at(level)) {
                        Some(Entry::Folder(folder)) => folder.place.clone(),
                        _ => None,
                    }
                })
                .collect(),
        }
    }

    /// The whole bar has been read off the disk again. Put this cursor back
    /// where it was standing, by the name of the column rather than by its
    /// number.
    ///
    /// The footing is what [`Self::remember`] took before the rebuild started.
    /// Everything the cursor has done since is discarded rather than corrected:
    /// a rebuild moves columns under it several times over — see
    /// [`Self::category_added`], which the shell calls for each column hung
    /// back on the bar — and the only account of where the user was standing
    /// that survives all of it is the one taken before any of it happened.
    ///
    /// Its own method rather than a call to [`Self::category_removed`] and
    /// [`Self::category_added`], because those answer *one* column coming or
    /// going and this is a list that may have changed in several places at
    /// once: turning the Steam integration off takes a column away and puts a
    /// row back in another one, which can bring a third column back from empty.
    ///
    /// The path *inside* the column is put back with it. A user who presses a
    /// row three levels down Settings is standing on that row when the bar is
    /// rebuilt under them, and a cursor thrown back to the top of the column
    /// would look like the press had gone wrong. What keeps that honest is
    /// [`Self::keep_in_bounds`], which the caller runs after this: the path is
    /// the one the user opened, and the rows on it are clamped to the columns
    /// as they are now.
    ///
    /// A bar that was gliding when it was rebuilt goes on gliding. The rebuild
    /// is not a move the user made, and a cursor that arrived at its column
    /// early because something was installed mid-stride would be the bar
    /// snapping out from under a press that had not finished.
    pub fn recall(&mut self, footing: &Footing, lattice: &Lattice) {
        // A bar with nothing on it is not something this shell can be built
        // from — every scan puts the Settings column up — and it is still what
        // every index below would be reaching into. Said as an early return
        // rather than left to `min`, which would clamp to row zero of a column
        // that is not there.
        if lattice.categories.is_empty() {
            return;
        }
        // What each column had been left on, under the name it goes by. A
        // column that was not there before this rebuild has been stood in by
        // nobody, which is what `None` means here and is not row zero of it —
        // see [`Self::selected_items`].
        self.selected_items = lattice
            .categories
            .iter()
            .map(|column| {
                footing
                    .remembered
                    .iter()
                    .find(|(name, _)| *name == column.id)
                    .and_then(|(_, row)| *row)
            })
            .collect();

        let last = lattice.categories.len().saturating_sub(1);
        let standing = footing
            .standing
            .and_then(|id| lattice.categories.iter().position(|column| column.id == id));
        // The column it was in has gone with the rebuild, which is what happens
        // to whoever is standing in the Steam library when the integration is
        // turned off. The column that closed the gap is where a struck-out line
        // leaves a finger, and it is the same answer [`Self::category_removed`]
        // gives.
        self.selected_category = standing.unwrap_or(footing.was).min(last);
        self.category_position = self.selected_category as f32;
        self.category_speed = 0.0;
        match standing {
            // The same column, whatever number it wears now: the path into it
            // is still the user's, and so is whatever stride the bar was in the
            // middle of.
            Some(_) => {
                self.category_position += footing.drift;
                self.category_speed = footing.speed;
                self.stack.clone_from(&footing.stack);
                self.open = footing.open;
            }
            // And if the column really did go, the path into it went with it: a
            // stack of rows belonging to a library that is no longer on the bar
            // is a path to nowhere.
            None => {
                self.leave_subcolumns();
                self.rest_on_first_row(lattice);
            }
        }
    }

    /// Open the folders somebody had open again, after a rebuild closed them.
    ///
    /// The second half of putting a cursor back, and the half that has to touch
    /// the disk. [`Self::recall`] puts the path back as a list of rows, and for
    /// every column on the bar but one that is the whole of it — the scan finds
    /// a category with its subcategories already under it. The explorer is the
    /// exception: what is under a folder is read when somebody opens it, so a
    /// bar rebuilt under a user standing three folders deep has nothing under
    /// any of them, and [`Self::keep_in_bounds`] does the only thing it can
    /// with a column that is not there and steps them out of all three.
    ///
    /// So the folders are opened again, in the order they were opened in the
    /// first place, and the rows the user had left in each are put back on top.
    /// One `readdir` per level on the thread that draws, which is
    /// [`Self::walk_to_file`]'s cost and is paid here only by somebody who was
    /// actually browsing when a package landed.
    ///
    /// A walk that does not arrive — a folder removed by the very thing that
    /// caused the rebuild — stops where it got to, which is a column the user
    /// can see and step out of rather than one that is not there.
    ///
    /// Answers what the column it ended in can be ordered by, like every other
    /// read of a folder, or `None` where there was nothing to walk.
    pub fn walk_back_in(
        &mut self,
        lattice: &mut Lattice,
        footing: &Footing,
        how: crate::files::How,
    ) -> Option<crate::media::Orders> {
        // Nothing was open, or the rebuild left it open. Asked of the depth
        // rather than of the rows, because this is about columns that are not
        // there at all.
        if footing.walked.is_empty() || self.open >= footing.open {
            return None;
        }
        // Out to the column's own rows first, whatever the clamp left standing:
        // the trail is walked from the top of it, and a cursor holding half a
        // path would be walking that trail from the middle.
        self.leave_subcolumns();
        let mut orders = None;
        for place in &footing.walked {
            let found = self
                .current_entries(lattice)
                .iter()
                .position(|entry| match entry {
                    Entry::Folder(folder) => folder.place.as_ref() == Some(place),
                    _ => false,
                });
            let Some(row) = found else {
                break;
            };
            self.point_at_row(row, lattice);
            let Some(read) = self.open_and_enter(lattice, how) else {
                break;
            };
            orders = Some(read);
        }
        // And the row in the innermost column, which is the only row on this
        // path the walk cannot know: every row above it is a folder the walk
        // has just opened, found by *what it is* rather than by where it used
        // to be, while the remembered numbers are from before the rebuild.
        //
        // Putting the whole remembered stack back undid the walk wherever a
        // listing had moved under it — and a rebuild is exactly when listings
        // move, since something landing on the disk is what caused it. One file
        // added to a folder on the path shifts every row below it, so the
        // restored number pointed at the folder *next door*, which the walk
        // never opened and so has no rows read under it, and
        // [`Self::keep_in_bounds`] can only answer a column that is not there
        // by stepping the user out of it and out of every column the walk had
        // just reopened under it. Somebody three folders deep came back one
        // folder deep, depending on what had been written where.
        //
        // Taken by its level rather than off the end of the stack, because the
        // stack outlives the walking: a column stepped *out* of leaves its row
        // behind so that stepping back in returns to it, so the last entry of a
        // remembered stack can belong to a column the user is not in. From the
        // innermost level down is the part this walk did not decide, and it
        // comes back whole. Where that innermost column has moved too, the
        // clamp answers for it, as it always did.
        if self.open == footing.open {
            if let Some(innermost) = self.open.checked_sub(1) {
                self.stack.truncate(innermost);
                self.stack
                    .extend_from_slice(footing.stack.get(innermost..).unwrap_or_default());
            }
        }
        self.keep_in_bounds(lattice);
        orders
    }

    /// Go to one column by name, from wherever the cursor is.
    ///
    /// Travelled rather than placed, unlike everything above: this is a move
    /// the *user* asked for — pressing the Steam row to be taken to their
    /// library — and the whole point of the bar sliding is that they can see
    /// where they were taken.
    pub fn select_category(&mut self, at: usize, lattice: &Lattice) {
        if at >= lattice.categories.len() || at == self.selected_category {
            return;
        }
        self.leave_subcolumns();
        self.selected_category = at;
        self.restore_column(lattice);
    }

    /// Put the cursor on this category's *own* column, wherever it was.
    ///
    /// [`Cursor::select_category`] answers a walk along the category row, and a
    /// walk that arrives where it started is not a move — so it returns without
    /// doing anything when the cursor is already in that category. Which is
    /// right for a walk and wrong for the shell reaching in and putting the
    /// cursor somewhere: "take them to the RetroArch column" has to mean the
    /// column and not the third subcolumn of it they happen to be standing in.
    ///
    /// The bug it was written for: a game that cannot start without a BIOS
    /// raises a panel offering to go and find one, and the row that asks is on
    /// the RetroArch column — two columns back out from the game whose press
    /// raised the panel. Pressing the offer did nothing at all.
    pub fn go_to_own_column(&mut self, at: usize, lattice: &Lattice) {
        if at >= lattice.categories.len() {
            return;
        }
        if at == self.selected_category {
            self.leave_subcolumns();
            self.restore_column(lattice);
            return;
        }
        self.select_category(at, lattice);
    }

    /// Come back out of every subcategory this cursor is standing in.
    fn leave_subcolumns(&mut self) {
        self.open = 0;
        self.stack.clear();
    }

    /// Step into the subcategory under the cursor. `false` if the row is not
    /// one — an application, or a setting, both of which are the end of a path.
    pub fn enter(&mut self, lattice: &Lattice) -> bool {
        let Some(entries) = self.current_entry(lattice).and_then(Entry::entries) else {
            return false;
        };
        // A subcategory with nothing in it is a dead end, and stepping into an
        // empty column would strand the cursor somewhere Back is the only way
        // out of.
        if entries.is_empty() {
            return false;
        }

        if self.stack.len() == self.open {
            // A list of values opens on the value in force, the way a settings
            // list does: the answer to "what is this set to" should be where
            // the cursor already is, not something to go looking for.
            //
            // Everything else opens on its first row — except that a shelf of
            // the user's own files carries the search above its files, and a
            // column of music opens on music. See [`crate::apps::head_rows`].
            let selected = entries
                .iter()
                .position(Entry::chosen)
                .unwrap_or_else(|| first_row(entries));
            self.stack.push(SubColumn {
                selected,
                // Placed, not travelled to: a column arrives by sliding in
                // from the side, and a row scrolling into place at the same
                // time would read as the list having moved under the user.
                position: selected as f32,
                speed: 0.0,
            });
        }
        self.open += 1;
        true
    }

    /// Step back out to the column this one was opened from. `false` at the
    /// top level, where there is nothing to step out of.
    pub fn leave(&mut self) -> bool {
        if self.open == 0 {
            return false;
        }
        self.open -= 1;
        true
    }

    /// Come back out of any column the catalogue no longer reaches.
    ///
    /// A cursor stands in a *path* — a category, a row of it, a row of that —
    /// and the catalogue under it is rewritten by things that have nothing to
    /// do with where anybody is standing: a scan answering, a package saying
    /// what it found, a folder being chosen out from under the very listing it
    /// was chosen in. A rewrite that shortens the path leaves this cursor
    /// standing at a depth the bar does not go to any more.
    ///
    /// Which is not a wrong row on screen but an empty screen. The whole chain
    /// of columns is drawn one step to the left per level opened — see
    /// [`crate::ui::bar_column_x`] — so a cursor two levels past the end of the
    /// path draws every column it still has two steps off the left edge, and
    /// what is left is the wallpaper with a shell running behind it. Nothing
    /// says so, nothing recovers, and Back is the only way out.
    ///
    /// So the depth is brought back to what the catalogue actually holds,
    /// every frame. `true` when it had to move, which is a frame to draw.
    pub fn settle(&mut self, lattice: &Lattice) -> bool {
        let mut moved = false;
        // An empty column counts as one that is not there, on the same terms
        // [`Self::enter`] refuses to step into one: a column with no rows is
        // somewhere the cursor cannot be, whether it was never opened or has
        // just been emptied.
        while self.open > 0
            && self
                .level_entries(lattice, self.open)
                .is_none_or(<[Entry]>::is_empty)
        {
            self.open -= 1;
            moved = true;
        }
        if moved {
            // The columns past where the cursor now stands are the ones being
            // stepped out of, and these were never stepped out of: they are
            // gone. Keeping them would leave the bar drawing a column of a path
            // that no longer exists.
            self.stack.truncate(self.open);
        }

        // And the columns kept for stepping straight back into, which are the
        // other half of the same problem and the harder half to see.
        //
        // A cursor that has come out of a path keeps where it was in each
        // column of it, so that going back in lands where it left. That memory
        // is a row *number*, and the rows it counted are not promised to still
        // be there — a folder chosen, a scan answering, a package arriving all
        // rewrite them. Step back in then and the column opens on a row that
        // does not exist: nothing under the highlight, and the list drawn
        // scrolled off its own top, which is a column of games with no game in
        // it. That is what a ROM folder chosen for the first time did, every
        // time, because choosing it is a walk several columns deep and the
        // column walked back out into is the one the walk had just replaced.
        for level in 0..self.stack.len() {
            let Some(entries) = self.level_entries(lattice, level + 1) else {
                break;
            };
            let Some(last) = entries.len().checked_sub(1) else {
                break;
            };
            let Some(column) = self.stack.get_mut(level) else {
                break;
            };
            if column.selected <= last {
                continue;
            }
            // Placed rather than travelled to, like every other arrival in a
            // column that is not on screen: the position is what the drawing
            // reads, and leaving it where it was is the whole of how a column
            // ends up showing its rows from somewhere off the top.
            column.selected = last;
            column.position = last as f32;
            column.speed = 0.0;
            moved = true;
        }

        // And the category's own column, which the stack does not hold: that
        // row lives in `selected_items`, one per category, and it goes stale
        // the same way for the same reasons. A console that stops needing its
        // BIOS takes the last row off the RetroArch column while somebody is
        // standing on it — they answered the question that row asked — and what
        // is left is a highlight over nothing, above a list drawn scrolled off
        // its own top.
        if self.open == 0 {
            let last = self
                .level_entries(lattice, 0)
                .and_then(|entries| entries.len().checked_sub(1));
            let standing = self.selected_items.get(self.selected_category).copied();
            if let (Some(last), Some(Some(row))) = (last, standing) {
                if row > last {
                    self.select_row(last);
                    self.item_position = last as f32;
                    self.item_speed = 0.0;
                    moved = true;
                }
            }
        }
        moved
    }

    /// Forget the columns kept for stepping straight back into.
    ///
    /// For a path that is not somewhere to go back to. Walking to a folder to
    /// answer a question is the case this exists for: the columns of that walk
    /// were a way of pointing at something, the question has been answered, and
    /// where the cursor stood in each of them is not a place anybody is
    /// returning to. See `Shell::leave_the_picker`.
    pub fn forget_the_way_back(&mut self) {
        self.stack.truncate(self.open);
    }

    /// Apply a navigation action. Returns `true` if anything moved.
    ///
    /// Launching is not here: it acts on the shared catalogue rather than on
    /// one display's view of it, so the shell drives it directly.
    ///
    /// Left and Right are the two arms of the cross at the top of a column,
    /// and the way out and in below it — the original bar's arrangement, where
    /// depth simply takes over the horizontal axis once there is no category
    /// row left to move along.
    ///
    /// Which means Right does *not* open a subcategory at the top level, and
    /// that is deliberate. The shell's own Settings column is subcategories
    /// all the way down, so a Right that opened one would be a Right that
    /// could never reach the column after Settings — the category row would be
    /// unreachable from the one column that is always there. Opening is
    /// [`Action::Launch`]'s job at the top level, as it was on the console,
    /// and Right's again from the moment there is a path to walk.
    pub fn navigate(&mut self, action: Action, lattice: &Lattice) -> bool {
        if lattice.categories.is_empty() {
            return false;
        }

        match action {
            Action::Left => {
                // Out of the path first: the category row is *behind* the
                // column the user is standing in, so reaching it means coming
                // back out to it.
                if self.leave() {
                    return true;
                }
                if self.selected_category == 0 {
                    return false;
                }
                self.selected_category -= 1;
                self.restore_column(lattice);
                true
            }
            Action::Right => {
                if self.open > 0 {
                    // Deeper, or nothing: inside a column the category row is
                    // not somewhere Right may jump to, because Left — the only
                    // way back — means something else in here.
                    return self.enter(lattice);
                }
                if self.selected_category + 1 >= lattice.categories.len() {
                    return false;
                }
                self.selected_category += 1;
                self.restore_column(lattice);
                true
            }
            Action::Up => {
                let current = self.selected_item();
                if current == 0 {
                    return false;
                }
                self.select_row(current - 1);
                true
            }
            Action::Down => {
                let count = self.current_entries(lattice).len();
                let current = self.selected_item();
                if current + 1 >= count {
                    return false;
                }
                self.select_row(current + 1);
                true
            }
            _ => false,
        }
    }

    /// Put the item column where the category being moved to left it, without
    /// travelling there.
    ///
    /// Each category remembers its own row, and easing to that row is the
    /// wrong reading of what happened: the column did not scroll while the
    /// user was away, it is a *different column*, and the one thing it should
    /// never do is replay a journey nobody made. The bar already fades the
    /// column out with the old category and in with the new one — this is what
    /// makes it fade back in at the row it was left on rather than sliding to
    /// it in plain sight.
    ///
    /// Invisible in practice: the fade is driven by the distance the category
    /// row still has to travel, which is at its greatest on the frame the
    /// press lands, so the column this moves is not on screen when it moves.
    ///
    /// The path taken through the old category goes with it. Its columns
    /// belong to a tree the bar has left, so they cannot be kept for the slide
    /// out the way a step back out keeps one — but the *depth* is left to ease
    /// back on its own rather than snapped, so the cross returns from wherever
    /// it had slid to instead of jumping there.
    ///
    /// A column nobody has been in yet has no row it was left on, and gets the
    /// head of its *list* — which is not row zero where something stands over
    /// the list. A Steam library carries its index above the first game, and a
    /// display arriving on that row would arrive on a control it did not ask
    /// for. See [`crate::apps::head_rows`].
    fn restore_column(&mut self, lattice: &Lattice) {
        self.stack.clear();
        self.open = 0;
        if self.row_never_chosen() {
            let row = first_row(self.current_entries(lattice));
            self.set_row_at(0, row);
        }
        self.item_position = self.selected_item() as f32;
        self.item_speed = 0.0;
    }

    /// Whether this cursor has yet to be put on a row of the column it is
    /// standing in. See [`Self::selected_items`], and [`Self::row_in_column`]
    /// for what the answer is used for elsewhere.
    fn row_never_chosen(&self) -> bool {
        self.selected_items
            .get(self.selected_category)
            .copied()
            .flatten()
            .is_none()
    }

    /// Advance the easing by `dt` seconds. Returns `true` while still moving,
    /// so the caller knows whether another frame is needed.
    ///
    /// The clock belongs to the caller, which keeps this deterministic and
    /// lets the shell drive it from its own frame timing.
    pub fn animate(&mut self, dt: f32) -> bool {
        let target_category = self.selected_category as f32;
        let target_item = self.row_at(0) as f32;
        let target_depth = self.open as f32;

        let step = |at: f32, speed: f32, target: f32, rate: f32| {
            let (at, speed) = lxb_protocol::overview::spring(
                at as f64,
                speed as f64,
                target as f64,
                rate as f64,
                dt as f64,
            );
            (at as f32, speed as f32)
        };
        (self.category_position, self.category_speed) = step(
            self.category_position,
            self.category_speed,
            target_category,
            EASE_RATE,
        );
        (self.item_position, self.item_speed) =
            step(self.item_position, self.item_speed, target_item, EASE_RATE);
        (self.depth_position, self.depth_speed) = step(
            self.depth_position,
            self.depth_speed,
            target_depth,
            DEPTH_EASE_RATE,
        );

        // Every column that is still on screen keeps easing, including the one
        // being stepped out of: a list that froze the moment it stopped being
        // the one in front would slide away mid-glide.
        let mut columns_moving = false;
        for column in &mut self.stack {
            let target = column.selected as f32;
            let close = settled_within(target);
            (column.position, column.speed) =
                step(column.position, column.speed, target, EASE_RATE);
            if (target - column.position).abs() > close || column.speed.abs() > close {
                columns_moving = true;
            } else {
                column.position = target;
                column.speed = 0.0;
            }
        }

        // Still moving while it is either away from its target or on its way
        // back to it: a spring an instant from crossing centre is at the
        // target and nowhere near finished.
        let item_close = settled_within(target_item);
        let moving = columns_moving
            || (target_category - self.category_position).abs() > SETTLED
            || (target_item - self.item_position).abs() > item_close
            || (target_depth - self.depth_position).abs() > SETTLED
            || self.category_speed.abs() > SETTLED
            || self.item_speed.abs() > item_close
            || self.depth_speed.abs() > SETTLED;

        if !moving {
            // Snap so nothing renders at a fractional offset forever.
            self.category_position = target_category;
            self.item_position = target_item;
            self.depth_position = target_depth;
            self.category_speed = 0.0;
            self.item_speed = 0.0;
            self.depth_speed = 0.0;
        }

        moving
    }
}

/// Which of the two controller lists a program is started with.
///
/// One program on this machine is not simply a reader of pads, and it is
/// exactly the one a shell with its Steam integration turned off starts like
/// anything else: Valve's client is the *other driver* of the pad this shell
/// reads from `hidraw`, and the only road a Steam game has to it. Handing it
/// the ordinary list takes that pad off the client altogether, and every game
/// it launches with it — which is a controller that quietly stops working, in a
/// session where nothing looks wrong.
///
/// So the two lists are named here rather than assumed, and the choice is made
/// where the program is known. See
/// [`crate::pad_guard::hidapi_ignore_list_for_valves_client`], where the
/// difference between them is argued and measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pads {
    /// Everything this shell starts, which reads controllers and nothing more.
    ForAnApplication,
    /// Valve's client, whichever way it was started: from its own `.desktop`
    /// entry on a bar with no Steam integration, or by the integration itself.
    ForValvesClient,
}

/// Start an application in an independent process session.
///
/// LineXinBar retains the returned handle only to collect its exit status. A
/// [`Child`] handle does not own the process lifetime: if the shell exits first,
/// the kernel reparents the running process and it continues normally.
pub fn launch(
    command: &str,
    terminal: bool,
    pads: Pads,
    wayland_display: &OsStr,
    xwayland_display: Option<&OsStr>,
) -> io::Result<Child> {
    let command = command.to_string();
    let argv = if terminal {
        terminal_argv(&command).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Terminal=true but no usable terminal emulator was found; set TERMINAL",
            )
        })?
    } else {
        vec![
            OsString::from("sh"),
            OsString::from("-c"),
            command.clone().into(),
        ]
    };

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    confine_to_session(&mut cmd, pads, wayland_display, xwayland_display);

    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    cmd.spawn()
}

/// Resolve a terminal and construct its argv without invoking another shell.
fn terminal_argv(command: &str) -> Option<Vec<OsString>> {
    let configured = std::env::var_os("TERMINAL");
    terminal_argv_with(
        configured.as_deref().and_then(OsStr::to_str),
        command,
        executable_on_path,
    )
}

/// Pure resolver used by tests with an injected executable lookup.
fn terminal_argv_with(
    configured: Option<&str>,
    command: &str,
    mut executable_exists: impl FnMut(&OsStr) -> bool,
) -> Option<Vec<OsString>> {
    if let Some(mut argv) = configured.and_then(split_words) {
        if argv
            .first()
            .is_some_and(|program| executable_exists(OsStr::new(program)))
        {
            let style = terminal_style(OsStr::new(&argv[0]));
            let argv = argv.drain(..).map(OsString::from).collect();
            return Some(append_terminal_command(argv, style, command));
        }
    }

    TERMINAL_CANDIDATES
        .iter()
        .find(|(program, _)| executable_exists(OsStr::new(program)))
        .map(|(program, style)| {
            append_terminal_command(vec![OsString::from(*program)], *style, command)
        })
}

fn terminal_style(program: &OsStr) -> TerminalExecStyle {
    let name = Path::new(program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match name.as_str() {
        "xdg-terminal-exec" | "kitty" | "kgx" | "ptyxis" | "gnome-terminal" => {
            TerminalExecStyle::DoubleDash
        }
        "konsole" => TerminalExecStyle::Konsole,
        "wezterm" => TerminalExecStyle::WezTerm,
        "xfce4-terminal" | "mate-terminal" | "terminator" => TerminalExecStyle::Execute,
        // `-e` is the long-established xterm convention and the best
        // interoperable choice for a custom `$TERMINAL` we do not recognise.
        _ => TerminalExecStyle::DashE,
    }
}

fn append_terminal_command(
    mut terminal: Vec<OsString>,
    style: TerminalExecStyle,
    command: &str,
) -> Vec<OsString> {
    match style {
        TerminalExecStyle::DashE => push_unless_last(&mut terminal, "-e"),
        TerminalExecStyle::DoubleDash => push_unless_last(&mut terminal, "--"),
        TerminalExecStyle::Execute => {
            if terminal
                .last()
                .is_none_or(|last| last != OsStr::new("-x") && last != OsStr::new("--execute"))
            {
                terminal.push(OsString::from("-x"));
            }
        }
        TerminalExecStyle::Konsole => {
            // Konsole may otherwise hand the request to an existing process
            // from the outer KDE session. Keep this window in LineXinBar's
            // confined display environment.
            let separate = terminal
                .iter()
                .any(|arg| arg == OsStr::new("--separate") || arg == OsStr::new("--nofork"));
            if !separate {
                let before_exec = terminal.last().is_some_and(|arg| arg == OsStr::new("-e"));
                let index = terminal.len() - usize::from(before_exec);
                terminal.insert(index, OsString::from("--separate"));
            }
            push_unless_last(&mut terminal, "-e");
        }
        TerminalExecStyle::WezTerm => {
            let has_start = terminal
                .iter()
                .skip(1)
                .any(|arg| arg == OsStr::new("start") || arg == OsStr::new("-e"));
            if !has_start {
                terminal.push(OsString::from("start"));
            }
            push_unless_last(&mut terminal, "--");
        }
    }

    terminal.extend([
        OsString::from("sh"),
        OsString::from("-c"),
        OsString::from(command),
    ]);
    terminal
}

fn push_unless_last(argv: &mut Vec<OsString>, value: &str) {
    if argv.last().is_none_or(|last| last != OsStr::new(value)) {
        argv.push(OsString::from(value));
    }
}

/// Minimal POSIX-style splitting of a command line into its words, including
/// quoted arguments. An unterminated quote or escape rejects the value, and the
/// caller falls back.
///
/// Used for `$TERMINAL`, and for reading which program an `Exec` line runs —
/// see [`crate::apps::App::window_names`]. The second is the same question
/// asked the same way on purpose: every `Exec` this shell starts goes through
/// `sh -c` (see [`launch`]), so this is how the line will really be read.
pub(crate) fn split_words(input: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut has_word = false;

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has_word {
                    out.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            '\'' | '"' => {
                let quote = c;
                has_word = true;
                loop {
                    match chars.next() {
                        Some(c) if c == quote => break,
                        None => return None,
                        Some('\\') if quote == '"' => current.push(chars.next()?),
                        Some(c) => current.push(c),
                    }
                }
            }
            '\\' => {
                has_word = true;
                current.push(chars.next()?);
            }
            c => {
                has_word = true;
                current.push(c);
            }
        }
    }

    if has_word {
        out.push(current);
    }
    Some(out)
}

/// Whether `program` is something this machine can actually run — a name found
/// on `PATH`, or a path that is there and executable.
pub fn executable_on_path(program: &OsStr) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return is_executable_file(path);
    }

    std::env::var_os("PATH").is_some_and(|search_path| {
        std::env::split_paths(&search_path)
            .any(|dir| is_executable_file(&dir.join(Path::new(program))))
    })
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Make an application use the same display servers as the shell, and the same
/// controllers.
///
/// In nested mode both `WAYLAND_SOCKET` and `DISPLAY` can refer to the host.
/// A connected `WAYLAND_SOCKET` cannot be reused by another client. `DISPLAY`
/// is accepted only through LineXinBar's explicit private-XWayland marker.
fn confine_to_session(
    command: &mut Command,
    pads: Pads,
    wayland_display: &OsStr,
    xwayland_display: Option<&OsStr>,
) {
    command
        .env("WAYLAND_DISPLAY", wayland_display)
        .env_remove("WAYLAND_SOCKET");

    if let Some(display) = xwayland_display {
        command
            .env("DISPLAY", display)
            .env("LXB_XWAYLAND_DISPLAY", display);
    } else {
        command
            .env_remove("DISPLAY")
            .env_remove("LXB_XWAYLAND_DISPLAY");
    }

    match pads {
        Pads::ForAnApplication => hide_guarded_pads_from_hidapi(command),
        Pads::ForValvesClient => hide_guarded_pads_from_valves_client(command),
    }
}

/// Tell SDL to read the pads this shell is guarding through `/dev/input`,
/// which is the only place it will find them without their guide button.
///
/// SDL prefers its own HIDAPI drivers to the kernel's gamepad node for the
/// controllers it recognises, and reads those over `hidraw`, where the grab in
/// [`crate::pad_guard`] does not reach — so a game would find the guide button
/// after all, on exactly the popular pads those drivers exist for. Named
/// devices are skipped by `SDL_hid_enumerate`, which is what every HIDAPI
/// driver is offered devices from.
///
/// Only pads the guard actually holds are named, so nothing is ever asked to
/// ignore the one route a controller has: a pad with no gamepad node is a pad
/// the guard never took, and it is not on this list.
///
/// Anything the user set is kept and added to rather than replaced. A person
/// who has told SDL to ignore a device has told it for a reason, and this is
/// not an argument with them.
pub(crate) fn hide_guarded_pads_from_hidapi(command: &mut Command) {
    ask_sdl_to_leave_them_alone(command, crate::pad_guard::hidapi_ignore_list());
}

/// The same for Valve's client, which is told about a shorter list.
///
/// One pad is missing from it: the Steam Controller this shell drives itself,
/// which Valve's client is the other driver of and every Steam game's only road
/// to. See [`crate::pad_guard::hidapi_ignore_list_for_valves_client`], which is
/// where that difference is argued.
pub(crate) fn hide_guarded_pads_from_valves_client(command: &mut Command) {
    ask_sdl_to_leave_them_alone(
        command,
        crate::pad_guard::hidapi_ignore_list_for_valves_client(),
    );
}

/// Put a list of pads in front of what is already there, or leave the
/// environment as it stands when there are none.
fn ask_sdl_to_leave_them_alone(command: &mut Command, guarded: Option<String>) {
    let Some(guarded) = guarded else {
        return;
    };
    let ignore = match std::env::var("SDL_HIDAPI_IGNORE_DEVICES") {
        Ok(theirs) if !theirs.trim().is_empty() => format!("{theirs},{guarded}"),
        _ => guarded,
    };
    command.env("SDL_HIDAPI_IGNORE_DEVICES", ignore);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn app(name: &str) -> App {
        App {
            name: name.into(),
            comment: None,
            icon: None,
            exec: "true".into(),
            terminal: false,
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            path: PathBuf::from("/tmp/x.desktop"),
            wm_class: None,
        }
    }

    /// A row that launches something.
    fn entry(name: &str) -> Entry {
        Entry::App(app(name))
    }

    /// A row that opens a column of its own.
    fn folder(title: &str, entries: Vec<Entry>) -> Entry {
        Entry::Folder(crate::apps::Folder {
            title_message: None,
            comment_message: None,
            identity: None,
            title: title.into(),
            comment: None,
            icon: None,
            entries,
            place: None,
            chosen: false,
            over_the_list: false,
            person: None,
            portrait: None,
            used: None,
        })
    }

    /// A subcategory that is also the answer in force — the one row shape that
    /// is both. See [`crate::apps::Folder::chosen`].
    fn chosen_folder(title: &str, entries: Vec<Entry>) -> Entry {
        let Entry::Folder(mut inner) = folder(title, entries) else {
            unreachable!("folder builds a folder");
        };
        inner.chosen = true;
        Entry::Folder(inner)
    }

    fn titles(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(Entry::title).collect()
    }

    /// A row that is one of a set of values, `chosen` if it is the one in
    /// force. It stands for an accent of its own name — the model does not
    /// care which setting a row carries, only that it carries one, so the
    /// title doubles as the value and a test can tell them apart.
    fn choice(title: &'static str, chosen: bool) -> Entry {
        Entry::Choice(crate::apps::Choice {
            title: title.into(),
            comment: None,
            icon: None,
            swatch: None,
            material: None,
            chosen,
            acts: false,
            setting: Some(Setting::Accent(title)),
            over_the_list: false,
        })
    }

    /// A row that does a thing rather than being one of a set of answers.
    fn acting(title: &'static str) -> Entry {
        Entry::Choice(crate::apps::Choice {
            title: title.into(),
            comment: None,
            icon: None,
            swatch: None,
            material: None,
            chosen: false,
            acts: true,
            setting: Some(Setting::Accent(title)),
            over_the_list: false,
        })
    }

    /// A value the shell can show but not change.
    fn reading(title: &str, chosen: bool) -> Entry {
        Entry::Choice(crate::apps::Choice {
            title: title.into(),
            comment: None,
            icon: None,
            swatch: None,
            material: None,
            chosen,
            acts: false,
            setting: None,
            over_the_list: false,
        })
    }

    fn model() -> Lattice {
        Lattice::with_wayland_display(
            vec![
                Category {
                    id: "a",
                    title: "A",
                    icon: "a",
                    entries: vec![entry("a1"), entry("a2"), entry("a3")],
                },
                Category {
                    id: "b",
                    title: "B",
                    icon: "b",
                    entries: vec![entry("b1")],
                },
            ],
            OsString::from("lxb-test"),
        )
    }

    /// A column with a path through it: one plain row, then a subcategory two
    /// levels deep whose innermost column is a list of values.
    fn nested() -> Lattice {
        Lattice::with_wayland_display(
            vec![
                Category {
                    id: "settings",
                    title: "Settings",
                    icon: "settings",
                    entries: vec![
                        entry("plain"),
                        folder(
                            "Appearance",
                            vec![folder(
                                "Accent colour",
                                vec![choice("Green", false), choice("Purple", true)],
                            )],
                        ),
                    ],
                },
                Category {
                    id: "b",
                    title: "B",
                    icon: "b",
                    entries: vec![entry("b1")],
                },
            ],
            OsString::from("lxb-test"),
        )
    }

    fn cursor(lattice: &Lattice) -> Cursor {
        Cursor::new(lattice.categories.len())
    }

    // --- the file explorer's columns ---------------------------------------

    /// A directory of this test's own under the system's temporary folder, or
    /// `None` where there is nowhere to write — in which case the test that
    /// wanted it says nothing rather than failing.
    fn scratch(name: &str) -> Option<std::path::PathBuf> {
        let dir = std::env::temp_dir().join(format!("lxb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// How every folder is read until somebody chooses otherwise: A to Z, with
    /// the names that begin with a dot left out.
    fn by_name() -> crate::files::How {
        crate::files::How::plain()
    }

    /// One column of two rows, both of them somewhere on the disk: the bar as
    /// it stands the moment somebody has stepped into Files.
    fn places(first: &Path, second: &Path) -> Lattice {
        let place = |title: &str, at: &Path| {
            Entry::Folder(crate::apps::Folder {
                title_message: None,
                comment_message: None,
                identity: None,
                title: title.into(),
                comment: None,
                icon: None,
                entries: Vec::new(),
                place: Some(crate::files::Place::Directory(
                    at.to_path_buf(),
                    crate::files::Shows::Everything,
                )),
                chosen: false,
                over_the_list: false,
                person: None,
                portrait: None,
                used: None,
            })
        };
        Lattice::with_wayland_display(
            vec![Category {
                id: "system",
                title: "System",
                icon: "system",
                entries: vec![place("First", first), place("Second", second)],
            }],
            OsString::from("lxb-test"),
        )
    }

    /// A folder is empty until it is pressed, and a press is what fills it.
    /// Nothing is read on the way past, which is the whole reason the reading
    /// can be done on the thread that draws.
    #[test]
    fn a_folder_is_read_on_the_press_that_opens_it() {
        let Some(dir) = scratch("open-place") else {
            return;
        };
        std::fs::create_dir(dir.join("inside")).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();

        let mut lattice = places(&dir, &dir);
        let mut cursor = cursor(&lattice);
        assert!(
            !cursor.enter(&lattice),
            "there is nothing in it to step into yet"
        );

        assert!(
            cursor.open_place(&mut lattice, by_name()).is_some(),
            "the press reads the folder"
        );
        assert!(cursor.enter(&lattice), "and now there is a column");
        let rows: Vec<&str> = cursor
            .current_entries(&lattice)
            .iter()
            .map(Entry::title)
            .collect();
        assert_eq!(
            rows,
            ["New folder", "Search", "inside", "a.txt"],
            "the field stands over the folder, and the row that makes one over that"
        );
        assert_eq!(
            cursor.selected_item(),
            2,
            "and the column opens on the folder, not on either row over it"
        );
        // What was found, on the row it was found under.
        assert_eq!(
            lattice.categories[0].entries[0].comment(),
            Some("1 folder, 1 file")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reading one folder drops what was read of the ones beside it. The tree
    /// holds the path the user is standing in; everywhere they have been is on
    /// the disk, where it can be read again.
    #[test]
    fn the_folders_beside_the_one_being_opened_give_up_their_rows() {
        let Some(dir) = scratch("siblings") else {
            return;
        };
        std::fs::write(dir.join("a.txt"), b"x").unwrap();

        let mut lattice = places(&dir, &dir);
        let mut cursor = cursor(&lattice);
        assert!(cursor.open_place(&mut lattice, by_name()).is_some());
        assert!(!lattice.categories[0].entries[0]
            .entries()
            .unwrap()
            .is_empty());

        assert!(
            cursor.navigate(Action::Down, &lattice),
            "down to the second"
        );
        assert!(cursor.open_place(&mut lattice, by_name()).is_some());
        assert!(
            lattice.categories[0].entries[0]
                .entries()
                .unwrap()
                .is_empty(),
            "the first has given up what it was holding"
        );
        assert!(!lattice.categories[0].entries[1]
            .entries()
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- being shown a file from outside the session -----------------------

    /// The bar as it stands at the start of a session: one category with the
    /// Files row at the head of it, and nothing read yet.
    fn with_a_files_row() -> Lattice {
        Lattice::with_wayland_display(
            vec![Category {
                id: "system",
                title: "System",
                icon: "system",
                entries: vec![Entry::Folder(crate::apps::Folder {
                    title_message: None,
                    comment_message: None,
                    identity: None,
                    title: "Files".into(),
                    comment: None,
                    icon: None,
                    entries: Vec::new(),
                    place: Some(crate::files::Place::Volumes(
                        crate::files::Shows::Everything,
                    )),
                    chosen: false,
                    over_the_list: false,
                    person: None,
                    portrait: None,
                    used: None,
                })],
            }],
            OsString::from("lxb-test"),
        )
    }

    /// The whole of what "Show in folder" asks for: the folder open, and the
    /// file itself under the cursor.
    #[test]
    fn a_file_is_shown_in_the_folder_that_holds_it() {
        let Some(dir) = scratch("show-in-folder") else {
            return;
        };
        let deep = dir.join("one").join("two");
        std::fs::create_dir_all(&deep).unwrap();
        for name in ["another.txt", "thing.zip"] {
            std::fs::write(deep.join(name), b"x").unwrap();
        }
        let thing = deep.join("thing.zip");

        let mut lattice = with_a_files_row();
        let mut cursor = cursor(&lattice);
        assert!(
            cursor
                .walk_to_file(&mut lattice, &deep, Some(&thing), by_name())
                .is_some(),
            "the walk arrives"
        );

        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("thing.zip"),
            "and leaves the file under the cursor"
        );
        // The trail behind it is the path: every column the walk opened is
        // still there, each keeping the row it was opened from, which is what
        // makes the path readable across the screen. Walked back out rather
        // than counted, because how many columns deep the folder is depends on
        // which disk it turned out to be under.
        assert!(cursor.leave(), "out to the folder holding it");
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("two")
        );
        assert!(cursor.leave(), "and out again");
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("one")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bar can be read off the disk again while somebody is browsing, and
    /// it closes every folder on it: what is under a folder is read when it is
    /// opened, so a scan finds the Files row with nothing but the disks under
    /// it. They are put back where they were standing rather than at the top of
    /// the column — a package landing is not a Back press.
    #[test]
    fn a_rebuild_puts_somebody_back_in_the_folder_they_were_browsing() {
        let Some(dir) = scratch("rebuilt-while-browsing") else {
            return;
        };
        let deep = dir.join("one").join("two");
        std::fs::create_dir_all(&deep).unwrap();
        for name in ["another.txt", "thing.zip"] {
            std::fs::write(deep.join(name), b"x").unwrap();
        }
        let thing = deep.join("thing.zip");

        let mut lattice = with_a_files_row();
        let mut cursor = cursor(&lattice);
        assert!(cursor
            .walk_to_file(&mut lattice, &deep, Some(&thing), by_name())
            .is_some());
        let depth = cursor.depth();
        let footing = cursor.remember(&lattice);

        // The rebuild: the same bar, read again, with every folder shut.
        let mut now = with_a_files_row();
        cursor.recall(&footing, &now);
        cursor.keep_in_bounds(&now);
        assert_eq!(cursor.depth(), 0, "the rebuild closed the path");

        assert!(
            cursor.walk_back_in(&mut now, &footing, by_name()).is_some(),
            "and the folders are opened again"
        );
        assert_eq!(cursor.depth(), depth, "as deep as they were");
        assert_eq!(
            cursor.current_entry(&now).map(Entry::title),
            Some("thing.zip"),
            "on the row they were on"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder on the path that gained a file while the bar was rebuilt does
    /// not take the path down with it.
    ///
    /// The rebuild happens *because* something landed on the disk, so a
    /// listing on the path having moved is the ordinary case rather than the
    /// odd one. The walk finds each folder by what it is, and the rows it
    /// lands on are the rebuilt listing's own — so the one thing that must not
    /// happen afterwards is those rows being replaced with the numbers they
    /// had before. They were, and a number one row out pointed at the folder
    /// next door: nothing has been read under that one, and a column with no
    /// rows is a column the cursor is stepped out of, taking every column the
    /// walk had just reopened with it. Somebody three folders deep came back
    /// one folder deep, at random, depending on what had been written where.
    ///
    /// It was found as a test that failed about one run in two — the walk
    /// below goes through `/tmp`, and a test suite makes and removes
    /// directories there while it runs.
    #[test]
    fn a_folder_that_moved_under_the_path_does_not_close_it() {
        let Some(dir) = scratch("moved-under-the-path") else {
            return;
        };
        let deep = dir.join("one").join("two");
        std::fs::create_dir_all(&deep).unwrap();
        for name in ["another.txt", "thing.zip"] {
            std::fs::write(deep.join(name), b"x").unwrap();
        }
        // Enough rows beside `two` that a row number taken from the column
        // below it lands on one of them rather than off the end, where the
        // clamp would quietly put it right again.
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(dir.join("one").join(name), b"x").unwrap();
        }
        let thing = deep.join("thing.zip");

        let mut lattice = with_a_files_row();
        let mut cursor = cursor(&lattice);
        assert!(cursor
            .walk_to_file(&mut lattice, &deep, Some(&thing), by_name())
            .is_some());
        let depth = cursor.depth();
        let footing = cursor.remember(&lattice);

        // What the rebuild is about: something landed on the disk, in a folder
        // the user has open. It sorts before `one`, so every row under it in
        // that column — the row the path goes through — is one further down
        // than the footing remembers.
        std::fs::create_dir(dir.join("0-just-arrived")).unwrap();

        let mut now = with_a_files_row();
        cursor.recall(&footing, &now);
        cursor.keep_in_bounds(&now);
        assert!(cursor.walk_back_in(&mut now, &footing, by_name()).is_some());
        assert_eq!(
            cursor.depth(),
            depth,
            "the walk found the folders and the rows it landed on are kept"
        );
        assert_eq!(
            cursor.current_entry(&now).map(Entry::title),
            Some("thing.zip"),
            "and the innermost row, which the walk cannot know, comes from the footing"
        );

        // And that row is taken by its level, not off the end of the stack. A
        // column stepped out of leaves its row behind so that stepping back in
        // returns to it, so somebody who walked six folders deep and pressed
        // Back is standing five deep with a stack six long — and the end of it
        // is a row in a column they are not in.
        assert!(cursor.leave());
        let depth = cursor.depth();
        let footing = cursor.remember(&now);
        std::fs::create_dir(dir.join("0-and-another")).unwrap();
        let mut later = with_a_files_row();
        cursor.recall(&footing, &later);
        cursor.keep_in_bounds(&later);
        assert!(cursor
            .walk_back_in(&mut later, &footing, by_name())
            .is_some());
        assert_eq!(cursor.depth(), depth);
        assert_eq!(
            cursor.current_entry(&later).map(Entry::title),
            Some("two"),
            "the row they were standing on, not the one they had stepped out of"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And nobody who was not browsing pays for it: a cursor standing anywhere
    /// but the explorer has no folders open, so there is nothing to read and
    /// the disk is not touched.
    #[test]
    fn a_rebuild_reads_no_folders_for_a_cursor_that_had_none_open() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.enter(&lattice), "into a subcategory of its own");
        let footing = cursor.remember(&lattice);

        let mut now = nested();
        cursor.recall(&footing, &now);
        cursor.keep_in_bounds(&now);
        assert_eq!(cursor.depth(), 1, "a subcategory comes back with the scan");
        assert!(
            cursor.walk_back_in(&mut now, &footing, by_name()).is_none(),
            "so there is nothing to walk"
        );
    }

    /// A folder asked for by itself is stood *in* rather than pointed at, and
    /// the column opens where any folder's does.
    #[test]
    fn a_folder_asked_for_alone_is_stood_in() {
        let Some(dir) = scratch("show-folder") else {
            return;
        };
        std::fs::write(dir.join("a.txt"), b"x").unwrap();

        let mut lattice = with_a_files_row();
        let mut cursor = cursor(&lattice);
        assert!(cursor
            .walk_to_file(&mut lattice, &dir, None, by_name())
            .is_some());

        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("a.txt"),
            "standing in the folder, on its first row"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder with nothing in it is stood in all the same. An unpacking
    /// arrives by this walk — see [`crate::Shell::stand_in_what_was_unpacked`]
    /// — and an archive holding one empty folder is a thing people make; the
    /// column it opens carries the explorer's own head rows, which is what
    /// keeps it from being the dead end [`Cursor::enter`] refuses.
    #[test]
    fn an_empty_folder_is_still_stood_in() {
        let Some(dir) = scratch("show-empty") else {
            return;
        };
        let empty = dir.join("nothing");
        std::fs::create_dir_all(&empty).unwrap();

        let mut lattice = with_a_files_row();
        let mut cursor = cursor(&lattice);
        assert!(
            cursor
                .walk_to_file(&mut lattice, &empty, None, by_name())
                .is_some(),
            "the walk arrives"
        );
        assert!(cursor.leave(), "and it was stood in, not pointed at");
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("nothing")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not there any more is still an arrival: the folder is
    /// what the user is shown, which is most of what was asked for.
    #[test]
    fn a_file_that_has_gone_still_opens_its_folder() {
        let Some(dir) = scratch("show-missing") else {
            return;
        };
        std::fs::write(dir.join("a.txt"), b"x").unwrap();

        let mut lattice = with_a_files_row();
        let mut cursor = cursor(&lattice);
        assert!(cursor
            .walk_to_file(&mut lattice, &dir, Some(&dir.join("gone.txt")), by_name())
            .is_some());
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("a.txt"),
            "the folder is open, on the row any folder opens on"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path on no disk this shell lists is no walk at all, and says so. The
    /// cursor is left in Files rather than put back — see the note on
    /// [`Cursor::walk_to_file`] — and nothing is shown moving, because the
    /// caller does not bring the bar forward for a walk that answered `None`.
    #[test]
    fn a_path_with_no_row_leading_to_it_is_refused() {
        let mut lattice = with_a_files_row();
        let mut cursor = cursor(&lattice);
        assert!(cursor
            .walk_to_file(
                &mut lattice,
                Path::new("/nowhere-on-this-disk/at/all"),
                None,
                by_name()
            )
            .is_none());
    }

    /// A bar with no Files row on it — which is what a session looks like
    /// before its catalogue has been built.
    #[test]
    fn there_is_no_walk_without_a_files_row() {
        assert_eq!(the_files_row(&model().categories), None);
    }

    /// `$HOME` is inside `/`, and a file in the user's own folder belongs
    /// under Home rather than four columns down from Root.
    #[test]
    fn the_deepest_disk_holding_a_path_wins() {
        let disk = |at: &str| {
            Entry::Folder(crate::apps::Folder {
                title_message: None,
                comment_message: None,
                identity: None,
                title: at.into(),
                comment: None,
                icon: None,
                entries: Vec::new(),
                place: Some(crate::files::Place::Directory(
                    PathBuf::from(at),
                    crate::files::Shows::Everything,
                )),
                chosen: false,
                over_the_list: false,
                person: None,
                portrait: None,
                used: None,
            })
        };
        let disks = [disk("/home/somebody"), disk("/"), disk("/run/media/stick")];
        assert_eq!(
            deepest_disk_holding(&disks, Path::new("/home/somebody/Downloads/thing.zip")),
            Some(PathBuf::from("/home/somebody"))
        );
        assert_eq!(
            deepest_disk_holding(&disks, Path::new("/usr/lib")),
            Some(PathBuf::from("/"))
        );
        assert_eq!(
            deepest_disk_holding(&disks, Path::new("/run/media/stick/photos")),
            Some(PathBuf::from("/run/media/stick"))
        );
    }

    /// The row a walk steps into is the one that *goes* there, not the one
    /// that reads like it.
    #[test]
    fn a_row_is_found_by_where_it_leads() {
        let Some(dir) = scratch("row-by-place") else {
            return;
        };
        std::fs::create_dir(dir.join("thing")).unwrap();
        std::fs::write(dir.join("thing.txt"), b"x").unwrap();

        let mut lattice = places(&dir, &dir);
        let mut cursor = cursor(&lattice);
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);
        let rows = cursor.current_entries(&lattice);

        let folder = row_leading_to(rows, &dir.join("thing")).expect("the folder");
        let file = row_leading_to(rows, &dir.join("thing.txt")).expect("the file");
        assert_ne!(folder, file);
        assert_eq!(rows[folder].title(), "thing");
        assert_eq!(rows[file].title(), "thing.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder read again opens at the top of itself rather than at the row
    /// number it was left on: the listing it came back with is not the listing
    /// that number was about.
    #[test]
    fn a_folder_read_again_opens_at_its_first_row() {
        let Some(dir) = scratch("reread") else {
            return;
        };
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let mut lattice = places(&dir, &dir);
        let mut cursor = cursor(&lattice);
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.navigate(Action::Down, &lattice);
        assert_eq!(cursor.selected_item(), 4);

        assert!(cursor.leave(), "back out to the folder it came from");
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);
        assert_eq!(
            cursor.selected_item(),
            2,
            "the first row of the listing, under the two rows that stand over it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The field at the head of a folder narrows the column it stands over —
    /// the one the cursor is standing *in*, which is the folder one level up.
    #[test]
    fn the_field_narrows_the_column_it_stands_over() {
        let Some(dir) = scratch("search-here") else {
            return;
        };
        for file in ["alpha.txt", "beta.txt", "another.txt"] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }

        let mut lattice = places(&dir, &dir);
        let mut cursor = cursor(&lattice);
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);

        assert!(cursor.search_here(&mut lattice, "alp", by_name()).is_some());
        let rows: Vec<&str> = cursor
            .current_entries(&lattice)
            .iter()
            .map(Entry::title)
            .collect();
        assert_eq!(rows, ["New folder", "alp", "Clear search", "alpha.txt"]);

        // And out again, without stepping anywhere: the row that empties the
        // field is the only thing that undoes one.
        assert!(cursor.search_here(&mut lattice, "", by_name()).is_some());
        assert_eq!(
            cursor.current_entries(&lattice).len(),
            5,
            "New folder, the field, and three"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A search belongs to the looking somebody is doing rather than to the
    /// folder: stepping out and back in is a fresh visit, and it opens on
    /// everything that is there.
    #[test]
    fn stepping_back_into_a_folder_does_not_inherit_the_last_search() {
        let Some(dir) = scratch("search-fresh") else {
            return;
        };
        for file in ["alpha.txt", "beta.txt"] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }

        let mut lattice = places(&dir, &dir);
        let mut cursor = cursor(&lattice);
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);
        cursor.search_here(&mut lattice, "alpha", by_name());
        assert_eq!(cursor.current_entries(&lattice).len(), 4);

        assert!(cursor.leave());
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);
        assert_eq!(
            cursor.current_entries(&lattice).len(),
            4,
            "New folder, the field, and both files — with the row that empties \
             the field gone, because nothing is being searched for"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every other row in the shell is left exactly as it was: this runs in
    /// front of both presses that open a column, and on all but the explorer's
    /// rows it has to do nothing at all.
    #[test]
    fn nothing_else_on_the_bar_is_read_off_the_disk() {
        let mut lattice = nested();
        let mut cursor = cursor(&lattice);
        assert!(
            cursor.open_place(&mut lattice, by_name()).is_none(),
            "a plain row"
        );
        cursor.navigate(Action::Down, &lattice);
        assert!(
            cursor.open_place(&mut lattice, by_name()).is_none(),
            "a subcategory of the shell's"
        );
        assert!(cursor.enter(&lattice), "which still opens");
    }

    /// A catalogue with one real application in it, filed inside a
    /// subcategory: nothing about a window says which column of the bar its
    /// application ended up in, so the lookup has to go all the way down.
    fn installed() -> Lattice {
        let mut browser = app("Firefox");
        browser.wm_class = Some("firefox".into());
        Lattice::with_wayland_display(
            vec![Category {
                id: "internet",
                title: "Internet",
                icon: "internet",
                entries: vec![folder("More", vec![Entry::App(browser)])],
            }],
            OsString::from("lxb-test"),
        )
    }

    /// What a window calls itself is a class; what the user calls it is the
    /// name on the desktop entry that installed it. This is the step between
    /// the two, and it is what lets a button say "Close Firefox".
    #[test]
    fn a_window_is_traced_back_to_the_application_that_installed_it() {
        let lattice = installed();

        assert_eq!(
            lattice
                .app_for_window("firefox")
                .map(|app| app.name.as_str()),
            Some("Firefox")
        );
        // The two spellings the same application arrives under elsewhere in
        // the shell: an X11 class is capitalised, and a reverse-DNS id carries
        // the name in its tail.
        assert_eq!(
            lattice
                .app_for_window("Firefox")
                .map(|app| app.name.as_str()),
            Some("Firefox")
        );
        assert_eq!(
            lattice
                .app_for_window("org.mozilla.firefox")
                .map(|app| app.name.as_str()),
            Some("Firefox")
        );

        // Nothing installed claims these, and neither may anything be invented
        // for them: the caller has its own answer for a window the catalogue
        // has never heard of.
        assert!(lattice.app_for_window("some-game").is_none());
        assert!(lattice.app_for_window("").is_none());
        assert!(lattice.app_for_window("   ").is_none());
    }

    /// An application with no tile is still an installed application, and the
    /// guide has to be able to name the window it puts on the screen. Without
    /// this the photo viewer's own window was traced back to nothing, and the
    /// entry that closes it read "Close Imagonsole" over a window the loading
    /// screen, the bar's menu and the desktop entry all call Pictures.
    #[test]
    fn and_so_is_a_window_of_an_application_that_was_taken_off_the_bar() {
        let mut lattice = installed();
        let mut viewer = app("Pictures");
        viewer.wm_class = Some("imagonsole".into());
        lattice.aside = vec![viewer];

        assert_eq!(
            lattice
                .app_for_window("imagonsole")
                .map(|app| app.name.as_str()),
            Some("Pictures")
        );
        // And it is still only that window: holding one aside does not make it
        // the answer for everything the tree has never heard of.
        assert!(lattice.app_for_window("some-game").is_none());
    }

    fn settle(cursor: &mut Cursor) {
        while cursor.animate(1.0 / 60.0) {}
    }

    #[test]
    fn navigates_within_bounds() {
        let lattice = model();
        let mut cursor = cursor(&lattice);

        // Cannot move before the first category or above the first item.
        assert!(!cursor.navigate(Action::Left, &lattice));
        assert!(!cursor.navigate(Action::Up, &lattice));

        assert!(cursor.navigate(Action::Down, &lattice));
        assert_eq!(cursor.selected_item(), 1);
        assert!(cursor.navigate(Action::Right, &lattice));
        assert_eq!(cursor.selected_category, 1);

        // Category B has a single app, so Down does nothing.
        assert!(!cursor.navigate(Action::Down, &lattice));
        assert!(!cursor.navigate(Action::Right, &lattice));
    }

    /// A pointer names the row it wants outright, where a direction can only
    /// ask for the next one — and the bar still travels there, so the click
    /// reads as the column being scrolled rather than replaced.
    #[test]
    fn a_row_can_be_pointed_at_directly() {
        let lattice = model();
        let mut cursor = cursor(&lattice);

        assert!(cursor.point_at_row(2, &lattice));
        assert_eq!(cursor.selected_item(), 2);
        assert!(cursor.item_position < 2.0, "and travels there");

        // The row it is already on is not a move, which is what tells a second
        // click on a row apart from the first.
        assert!(!cursor.point_at_row(2, &lattice));
        // Nor is a row the column does not have.
        assert!(!cursor.point_at_row(9, &lattice));
        assert_eq!(cursor.selected_item(), 2);
    }

    /// Pointing at a category steps out of whatever path is open first, exactly
    /// as walking left to the row would: arriving with a path still open would
    /// leave a trail belonging to a category the user has left.
    #[test]
    fn pointing_at_a_category_leaves_the_path_behind() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.enter(&lattice));
        assert_eq!(cursor.depth(), 1);

        assert!(cursor.point_at_category(1, &lattice));
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);

        // The category it is already on, with nothing open, is not a move.
        assert!(!cursor.point_at_category(1, &lattice));
        assert!(!cursor.point_at_category(7, &lattice));
    }

    /// The one case where pointing at the category already selected *is* a
    /// move: the cursor is inside a path hanging off it, and the button at the
    /// head of that trail is the way back out.
    #[test]
    fn pointing_at_the_open_categorys_button_walks_back_out_to_it() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.enter(&lattice));

        assert!(cursor.point_at_category(cursor.selected_category, &lattice));
        assert_eq!(cursor.depth(), 0);
    }

    #[test]
    fn remembers_selection_per_category() {
        let lattice = model();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.navigate(Action::Down, &lattice);
        assert_eq!(cursor.selected_item(), 2);

        cursor.navigate(Action::Right, &lattice);
        assert_eq!(cursor.selected_item(), 0);

        cursor.navigate(Action::Left, &lattice);
        assert_eq!(cursor.selected_item(), 2, "selection should be restored");
    }

    /// Restored, not travelled to. A remembered row that the column scrolls
    /// down to on the way back is the bar claiming something moved while the
    /// user was in another category, which is not what happened.
    #[test]
    fn a_remembered_row_is_already_there_rather_than_scrolled_to() {
        let lattice = model();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.navigate(Action::Down, &lattice);
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.item_position, 2.0);

        // Away: the column belongs to the next category from the first frame.
        cursor.navigate(Action::Right, &lattice);
        assert_eq!(cursor.item_position, 0.0, "and not on its way there");

        // And back, with nothing left to animate vertically — only the
        // category row is still travelling.
        cursor.navigate(Action::Left, &lattice);
        assert_eq!(cursor.item_position, 2.0);
        cursor.animate(1.0 / 60.0);
        assert_eq!(cursor.item_position, 2.0, "it must not drift off the row");

        // Moving inside a category still glides: this is about crossing
        // between them, not about killing the bar's vertical easing.
        assert!(cursor.navigate(Action::Up, &lattice));
        cursor.animate(1.0 / 60.0);
        assert!(
            cursor.item_position < 2.0 && cursor.item_position > 1.0,
            "a step up the column should still travel: {}",
            cursor.item_position
        );
    }

    /// Below the top level Right is the way in and Left the way out, and the
    /// two are exact opposites: no sequence of them can leave the cursor
    /// somewhere the other cannot undo.
    #[test]
    fn a_path_is_walked_in_with_right_and_out_with_left() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);

        // At the top of a column the horizontal axis still belongs to the
        // category row, whatever kind of row the cursor is on.
        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.navigate(Action::Right, &lattice));
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);
        assert!(cursor.navigate(Action::Left, &lattice));

        // Opening is Launch's job there — and from then on the axis is depth.
        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.enter(&lattice), "Appearance opens");
        assert_eq!(cursor.depth(), 1);
        assert_eq!(
            cursor.selected_category, 0,
            "opening a subcategory must not also move along the row"
        );
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Accent colour")
        );

        assert!(cursor.navigate(Action::Right, &lattice));
        assert_eq!(cursor.depth(), 2);
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Purple")
        );

        // A value is the end of the path: there is nothing further right, and
        // the category row is not reachable from inside a column.
        assert!(!cursor.navigate(Action::Right, &lattice));
        assert_eq!(cursor.selected_category, 0);

        // And back out, one level per press, to the row it was opened from.
        assert!(cursor.navigate(Action::Left, &lattice));
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Accent colour")
        );
        assert!(cursor.navigate(Action::Left, &lattice));
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Appearance")
        );
        assert_eq!(cursor.depth(), 0);
        // Only now does Left mean the category row again.
        assert!(!cursor.navigate(Action::Left, &lattice));
    }

    /// A subcategory with nothing in it is a row, not a way in. Multimedia
    /// ships two of them — Music and Video, empty until the walk over the
    /// user's home directory finds something to hang on them — so this is what
    /// the bar does in the first seconds of a session on a machine with no
    /// music on it, rather than a case that cannot arise.
    ///
    /// What must not happen is the cursor landing in a column with no rows: it
    /// would be standing on nothing, with Back the only key that did anything
    /// and no row under the light to say why.
    #[test]
    fn an_empty_subcategory_is_a_dead_end_rather_than_an_empty_column() {
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "multimedia",
                title: "Multimedia",
                icon: "multimedia",
                entries: vec![folder("Music", Vec::new()), entry("Audacity")],
            }],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&lattice);
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Music")
        );

        assert!(
            !cursor.enter(&lattice),
            "there is nothing in there to step into"
        );
        assert_eq!(cursor.depth(), 0);
        assert_eq!(cursor.columns(&lattice).len(), 1);

        // And the row is still the one under the light, so the column the user
        // is looking at is the column they were looking at.
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Music")
        );
        assert!(cursor.navigate(Action::Down, &lattice));
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Audacity")
        );
    }

    /// One of the user's own files, as the shelf holds it.
    fn shelved(path: &str) -> crate::media::Shelved {
        Arc::new(crate::media::File::at(Path::new(path)).expect("a listable file"))
    }

    /// A row standing for it. Rebuilding a column means new rows for the *same*
    /// files, which is what the cursor follows — see [`Cursor::keep_on_media`].
    fn song(file: &crate::media::Shelved) -> Entry {
        Entry::Media(Arc::clone(file))
    }

    /// Music arrives all through a session and arrives in alphabetical order,
    /// so a track found now lands *above* the one the user is looking at as
    /// often as below it. The cursor has to stay on the song, not on the row
    /// number — otherwise the shell would be quietly changing what Accept is
    /// about while somebody reads the screen.
    #[test]
    fn a_song_arriving_does_not_move_the_one_under_the_cursor() {
        let column = |songs: Vec<Entry>| {
            Lattice::with_wayland_display(
                vec![Category {
                    id: "multimedia",
                    title: "Multimedia",
                    icon: "multimedia",
                    entries: vec![folder("Music", songs)],
                }],
                OsString::from("lxb-test"),
            )
        };
        let (alpha, beta) = (shelved("/m/alpha.mp3"), shelved("/m/beta.mp3"));
        let (delta, epsilon) = (shelved("/m/delta.mp3"), shelved("/m/epsilon.mp3"));
        let lattice = column(vec![song(&beta), song(&delta)]);
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        assert!(cursor.navigate(Action::Down, &lattice));
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("delta")
        );
        while cursor.animate(1.0 / 60.0) {}
        let settled = cursor.position_at(1);

        // Two more turn up, one of them above the row being read.
        let lattice = column(vec![
            song(&alpha),
            song(&beta),
            song(&delta),
            song(&epsilon),
        ]);
        cursor.keep_on_media(&lattice, &delta);

        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("delta")
        );
        assert_eq!(cursor.selected_item(), 2, "one row further down the list");
        // And the column moved with it, so nothing is left travelling: the
        // list slid under a cursor that never went anywhere.
        assert_eq!(cursor.position_at(1), settled + 1.0);
        assert!(!cursor.animate(1.0 / 60.0), "nothing left to ease");
    }

    /// A row in somebody's Steam library.
    fn game(app_id: u32, name: &str, installed: bool) -> Entry {
        Entry::Game(crate::apps::Game {
            ways: 1,
            progress: None,
            app_id,
            name: name.into(),
            note: String::new(),
            installed,
            updating: false,
            steam_client: true,
            standing: lxb_steam::library::Standing::Ready,
            stuck: false,
            waiting_for_steam: false,
        })
    }

    /// The row a Steam library carries over its first game.
    fn index(games: Vec<Entry>) -> Entry {
        Entry::Folder(crate::apps::Folder {
            title_message: None,
            comment_message: None,
            identity: None,
            title: "Alphabetical".into(),
            comment: None,
            icon: None,
            entries: games,
            place: None,
            chosen: false,
            over_the_list: true,
            person: None,
            portrait: None,
            used: None,
        })
    }

    /// A library with these games in it, in this order.
    fn library(games: Vec<Entry>) -> Lattice {
        Lattice::with_wayland_display(
            vec![Category {
                id: "steam",
                title: "Steam",
                icon: "steam",
                entries: games,
            }],
            OsString::from("lxb-test"),
        )
    }

    /// The bug this pins: waiting on a download at the game's own row, and
    /// being left looking at a different game the moment it finished.
    ///
    /// The library is installed-first, so a game that lands on the disk moves
    /// from the bottom half of the list to the top — halfway up a list of
    /// hundreds. Nothing about that is a move the user made, so the cursor
    /// follows the game and the column is drawn from the same place: the same
    /// cover is under the same highlight on the frame after as on the frame
    /// before.
    #[test]
    fn a_game_that_finishes_downloading_keeps_the_cursor_it_was_under() {
        let lattice = library(vec![
            game(1, "Aeonic", true),
            game(2, "Zenith", true),
            game(3, "Celeste", false),
            game(4, "Downloading", false),
        ]);
        let mut cursor = cursor(&lattice);
        for _ in 0..3 {
            assert!(cursor.navigate(Action::Down, &lattice));
        }
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.game_in_column(&lattice, 0), Some(4));
        let settled = cursor.position_at(0);

        // It lands, and the library comes back re-sorted around it.
        let lattice = library(vec![
            game(1, "Aeonic", true),
            game(4, "Downloading", true),
            game(2, "Zenith", true),
            game(3, "Celeste", false),
        ]);
        cursor.keep_on_game(&lattice, 0, 4);

        assert_eq!(cursor.selected_item(), 1, "two rows up the list");
        assert_eq!(cursor.game_in_column(&lattice, 0), Some(4));
        assert_eq!(
            cursor.position_at(0),
            settled - 2.0,
            "and the column is drawn from two rows further up, so nothing moved"
        );
        assert!(!cursor.animate(1.0 / 60.0), "nothing left to ease");
    }

    /// The Epic Games column re-sorts the same way when a game lands, and its
    /// games are known by Epic's name for them rather than by a number.
    #[test]
    fn an_epic_game_that_finishes_installing_keeps_the_cursor_too() {
        let epic = |app: &str, installed: bool| {
            Entry::EpicGame(crate::apps::EpicGame {
                app_name: app.to_string(),
                name: app.to_string(),
                note: String::new(),
                progress: None,
                installed,
                start: None,
                cover: None,
                shape: None,
                hero: None,
                logo: None,
            })
        };
        let column = |entries| {
            Lattice::with_wayland_display(
                vec![Category {
                    id: "epic",
                    title: "Epic Games",
                    icon: "lxb:epic",
                    entries,
                }],
                OsString::from("lxb-test"),
            )
        };
        let lattice = column(vec![epic("Owl", false), epic("Quail", false)]);
        let mut cursor = cursor(&lattice);
        assert!(cursor.navigate(Action::Down, &lattice));
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(
            cursor.epic_game_in_column(&lattice, 0).as_deref(),
            Some("Quail")
        );

        let lattice = column(vec![epic("Quail", true), epic("Owl", false)]);
        cursor.keep_on_epic_game(&lattice, 0, "Quail");
        assert_eq!(cursor.selected_item(), 0);
        assert_eq!(
            cursor.epic_game_in_column(&lattice, 0).as_deref(),
            Some("Quail")
        );
    }

    /// The same for a display that is somewhere else entirely. The row a
    /// cursor is on in a category it is not standing in is remembered, and
    /// walking back to a library that re-sorted in the meantime would land on
    /// a different game for exactly the same reason.
    #[test]
    fn a_display_looking_elsewhere_comes_back_to_the_game_it_left() {
        let games = |sorted: bool| {
            let mut categories = vec![Category {
                id: "applications",
                title: "Applications",
                icon: "applications",
                entries: vec![entry("Audacity")],
            }];
            categories.push(Category {
                id: "steam",
                title: "Steam",
                icon: "steam",
                entries: if sorted {
                    vec![game(9, "Fetched", true), game(1, "Aeonic", false)]
                } else {
                    vec![game(1, "Aeonic", false), game(9, "Fetched", false)]
                },
            });
            Lattice::with_wayland_display(categories, OsString::from("lxb-test"))
        };
        let lattice = games(false);
        let mut cursor = cursor(&lattice);
        cursor.select_category(1, &lattice);
        assert!(cursor.navigate(Action::Down, &lattice));
        cursor.select_category(0, &lattice);
        let elsewhere = cursor.position_at(0);

        let lattice = games(true);
        cursor.keep_on_game(&lattice, 1, 9);

        cursor.select_category(1, &lattice);
        assert_eq!(cursor.game_in_column(&lattice, 1), Some(9));
        assert_eq!(
            elsewhere,
            cursor.position_at(0),
            "and the column the display is actually in was not moved under it"
        );
    }

    /// A column that carries something over its list opens on the list.
    ///
    /// The Steam library hangs its index above its first game, and Up from that
    /// game is what reaches it — the place anything standing over a list stands.
    /// A display that arrived on the index instead would begin every visit to
    /// somebody's library by stepping down off a row they did not ask for, and
    /// this is the same rule a shelf's search field is under.
    #[test]
    fn a_column_with_a_row_over_it_opens_on_the_first_game() {
        let lattice = Lattice::with_wayland_display(
            vec![
                Category {
                    id: "applications",
                    title: "Applications",
                    icon: "applications",
                    entries: vec![entry("Audacity")],
                },
                Category {
                    id: "steam",
                    title: "Steam",
                    icon: "steam",
                    entries: vec![
                        index(vec![game(1, "Aeonic", false)]),
                        game(1, "Aeonic", false),
                        game(2, "Zenith", true),
                    ],
                },
            ],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&lattice);
        cursor.select_category(1, &lattice);

        assert_eq!(cursor.selected_item(), 1);
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Aeonic"),
            "the first game, not the row over it"
        );
        assert_eq!(
            cursor.position_at(0),
            1.0,
            "and the column is drawn from there rather than sliding down to it"
        );

        // The index is where anything standing over a list is: one press up.
        assert!(cursor.navigate(Action::Up, &lattice));
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Alphabetical")
        );
    }

    /// The bug this pins: a library that arrives in two parts while nobody is
    /// looking at it, and a display that had never opened Steam walking in on
    /// the middle of it.
    ///
    /// Steam answers "what is installed" off this machine's own disk in a
    /// moment and "what does this account own" a good deal later, so the column
    /// exists — listed in whatever order the user chose — with only the
    /// installed games in it. A cursor that read its slot as a row would be
    /// standing on the head of that half-built list, would follow that game to
    /// wherever the finished order puts it, and would open there: which under
    /// "Recently played" is a game chosen by what happens to be on the disk,
    /// somewhere down a list whose top the user has never seen.
    #[test]
    fn a_library_that_fills_in_leaves_a_display_that_never_opened_it_at_the_head() {
        let steam = |entries: Vec<Entry>| {
            Lattice::with_wayland_display(
                vec![
                    Category {
                        id: "applications",
                        title: "Applications",
                        icon: "applications",
                        entries: vec![entry("Audacity")],
                    },
                    Category {
                        id: "steam",
                        title: "Steam",
                        icon: "steam",
                        entries,
                    },
                ],
                OsString::from("lxb-test"),
            )
        };

        // What the disk knows on its own, which is only what is installed.
        let lattice = steam(vec![game(9, "Fetched", true)]);
        let mut cursor = cursor(&lattice);
        assert_eq!(
            cursor.selected_category, 0,
            "the display is in another column"
        );
        assert_eq!(
            cursor.game_in_column(&lattice, 1),
            None,
            "and so it is on no game in this one"
        );

        // And then the account's library, in an order that has nothing to do
        // with what is on the disk.
        let lattice = steam(vec![
            game(1, "Aeonic", false),
            game(2, "Zenith", false),
            game(9, "Fetched", true),
        ]);
        cursor.keep_on_game(&lattice, 1, 9);

        cursor.select_category(1, &lattice);
        assert_eq!(cursor.selected_item(), 0);
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Aeonic"),
            "the head of the order that was asked for"
        );
        assert_eq!(
            cursor.position_at(0),
            0.0,
            "and the column is drawn from the top rather than scrolled into"
        );
    }

    /// The other half of that rule: a display that *is* in the library when the
    /// rest of it lands keeps the game under its highlight, even though it
    /// never moved off the head of the column.
    ///
    /// Somebody is looking at that row. The list filling in underneath it is
    /// not a move they made, and the cover under the highlight becoming a
    /// different game is the thing [`Cursor::keep_on_game`] exists to prevent.
    #[test]
    fn a_display_standing_in_the_library_keeps_its_game_when_the_rest_arrives() {
        let lattice = library(vec![game(9, "Fetched", true)]);
        let mut cursor = cursor(&lattice);
        assert_eq!(cursor.game_in_column(&lattice, 0), Some(9));

        let lattice = library(vec![
            game(1, "Aeonic", false),
            game(2, "Zenith", false),
            game(9, "Fetched", true),
        ]);
        cursor.keep_on_game(&lattice, 0, 9);

        assert_eq!(cursor.selected_item(), 2, "the same game, further down");
        assert_eq!(
            cursor.position_at(0),
            2.0,
            "and the column drawn from two rows further down, so nothing moved"
        );
        assert!(!cursor.animate(1.0 / 60.0), "nothing left to ease");
    }

    /// A letter of the index re-sorts when a download lands — installed games
    /// come first in it, as they do in the library — and the display watching
    /// that download stays on the game rather than on the row number.
    #[test]
    fn a_game_that_lands_inside_a_letter_keeps_the_cursor_that_was_on_it() {
        let letter = |games: Vec<Entry>| {
            Lattice::with_wayland_display(
                vec![Category {
                    id: "steam",
                    title: "Steam",
                    icon: "steam",
                    entries: vec![index(games.clone())],
                }],
                OsString::from("lxb-test"),
            )
        };
        let lattice = letter(vec![
            game(1, "Aeonic", true),
            game(2, "Alpha", false),
            game(3, "Anvil", false),
        ]);
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice), "into the index");
        assert!(cursor.navigate(Action::Down, &lattice));
        assert!(cursor.navigate(Action::Down, &lattice));
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.game_inside(&lattice), Some(3));
        let settled = cursor.position_at(1);

        // Anvil finishes, and the letter comes back with it at the top.
        let lattice = letter(vec![
            game(3, "Anvil", true),
            game(1, "Aeonic", true),
            game(2, "Alpha", false),
        ]);
        cursor.keep_inside_on_game(&lattice, 3);

        assert_eq!(cursor.selected_item(), 0, "two rows up its letter");
        assert_eq!(cursor.game_inside(&lattice), Some(3));
        assert_eq!(
            cursor.position_at(1),
            settled - 2.0,
            "and the column is drawn from two rows further up, so nothing moved"
        );
        assert!(!cursor.animate(1.0 / 60.0), "nothing left to ease");
    }

    /// The same question asked of a cursor that has stepped into nothing
    /// answers nothing: the category's own column is [`Cursor::game_in_column`]'s
    /// to keep, and two rules over one row would fight.
    #[test]
    fn a_cursor_at_the_top_of_a_column_is_inside_no_letter() {
        let lattice = library(vec![game(1, "Aeonic", true), game(2, "Zenith", false)]);
        let cursor = cursor(&lattice);
        assert_eq!(cursor.game_inside(&lattice), None);
    }

    /// A game that has left the library altogether — a shared title whose
    /// lender took it back — leaves the cursor where it is standing, the same
    /// answer a deleted file gets.
    #[test]
    fn a_game_that_leaves_the_library_leaves_the_cursor_where_it_stands() {
        let lattice = library(vec![game(1, "Aeonic", true), game(2, "Zenith", true)]);
        let mut cursor = cursor(&lattice);
        assert!(cursor.navigate(Action::Down, &lattice));

        let lattice = library(vec![game(1, "Aeonic", true), game(3, "Celeste", true)]);
        cursor.keep_on_game(&lattice, 0, 2);
        assert_eq!(cursor.selected_item(), 1);
        assert_eq!(cursor.game_in_column(&lattice, 0), Some(3));
    }

    /// A file that has gone off the disk takes its row with it, and the cursor
    /// stays where it is standing rather than following the file into nothing.
    #[test]
    fn a_song_deleted_from_under_the_cursor_leaves_it_where_it_stands() {
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "multimedia",
                title: "Multimedia",
                icon: "multimedia",
                entries: vec![folder(
                    "Music",
                    vec![song(&shelved("/m/a.mp3")), song(&shelved("/m/c.mp3"))],
                )],
            }],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        assert!(cursor.navigate(Action::Down, &lattice));

        cursor.keep_on_media(&lattice, &shelved("/m/b.mp3"));
        assert_eq!(cursor.selected_item(), 1);
        assert_eq!(cursor.current_entry(&lattice).map(Entry::title), Some("c"));
    }

    /// A shelf listed in a different order is a different list, so the cursor
    /// goes to the head of it — placed there, with nothing left travelling.
    #[test]
    fn a_reordered_column_is_shown_from_its_first_row() {
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "multimedia",
                title: "Multimedia",
                icon: "multimedia",
                entries: vec![folder(
                    "Music",
                    vec![
                        song(&shelved("/m/a.mp3")),
                        song(&shelved("/m/b.mp3")),
                        song(&shelved("/m/c.mp3")),
                    ],
                )],
            }],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        assert!(cursor.navigate(Action::Down, &lattice));
        assert!(cursor.navigate(Action::Down, &lattice));
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.selected_item(), 2);

        cursor.rest_on_first_row(&lattice);
        assert_eq!(cursor.selected_item(), 0);
        assert_eq!(cursor.current_entry(&lattice).map(Entry::title), Some("a"));
        // Placed, not travelled to: there is no journey through a list whose
        // every row has just changed.
        assert_eq!(cursor.position_at(1), 0.0);
        assert!(!cursor.animate(1.0 / 60.0), "nothing left to ease");
    }

    /// A shelf of the user's own files, as the worker hands it over: the search
    /// at the head of it, and then the files.
    /// `paths` is what the search has left; `found` is how many are on the
    /// shelf altogether, which is not the same number once one is running.
    fn shelf_of(query: &str, paths: &[&str], found: usize) -> Lattice {
        let listing: Vec<crate::media::Shelved> = paths.iter().map(|path| shelved(path)).collect();
        Lattice::with_wayland_display(
            vec![Category {
                id: "multimedia",
                title: "Multimedia",
                icon: "multimedia",
                entries: vec![Entry::Folder(crate::apps::Folder {
                    title_message: None,
                    comment_message: None,
                    identity: None,
                    title: "Music".into(),
                    comment: None,
                    icon: Some("music".into()),
                    entries: crate::apps::media_rows(
                        listing,
                        crate::media::Kind::Audio,
                        query,
                        found,
                    ),
                    place: None,
                    chosen: false,
                    over_the_list: false,
                    person: None,
                    portrait: None,
                    used: None,
                })],
            }],
            OsString::from("lxb-test"),
        )
    }

    /// A column of music opens on music. The field is above the first file
    /// rather than in front of it: every visit to a shelf would otherwise begin
    /// by stepping over a control nobody asked for.
    #[test]
    fn a_shelf_opens_on_its_first_file_and_not_on_its_search() {
        let lattice = shelf_of("", &["/m/a.mp3", "/m/b.mp3"], 2);
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        assert_eq!(cursor.selected_item(), 1);
        assert_eq!(cursor.current_entry(&lattice).map(Entry::title), Some("a"));

        // And the field is exactly one press of Up away, which is where a
        // person looks for the thing above the first thing.
        assert!(cursor.navigate(Action::Up, &lattice));
        assert!(cursor
            .current_entry(&lattice)
            .and_then(Entry::search)
            .is_some());
        assert!(
            !cursor.navigate(Action::Up, &lattice),
            "and nothing above that"
        );

        // Reordering the shelf brings the cursor back to the first file for the
        // same reason: what "newest first" asks to be shown is the newest file.
        cursor.navigate(Action::Down, &lattice);
        cursor.navigate(Action::Down, &lattice);
        while cursor.animate(1.0 / 60.0) {}
        cursor.rest_on_first_row(&lattice);
        assert_eq!(cursor.current_entry(&lattice).map(Entry::title), Some("a"));
        assert!(!cursor.animate(1.0 / 60.0), "placed, not travelled to");

        // A column with no such row is untouched by any of it.
        let plain = installed();
        let mut walker = Cursor::new(plain.categories.len());
        assert!(walker.enter(&plain));
        assert_eq!(walker.selected_item(), 0);
    }

    /// A search narrowed down to nothing still opens somewhere the user can act
    /// from, rather than on a row that is not there.
    #[test]
    fn a_shelf_with_no_matches_opens_on_the_way_out_of_the_search() {
        let lattice = shelf_of("zzz", &[], 40);
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice), "the two rows are still a column");
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("Clear search")
        );
    }

    /// The condition the shell watches to know a search field has been left:
    /// the cursor is simply no longer standing on it. The bar can be walked out
    /// from under an open keyboard by routes that never touch the field — a
    /// pointer resting on the category row is one — so what ends the typing is
    /// this, asked once a frame, rather than any particular way of leaving.
    #[test]
    fn walking_out_to_the_categories_leaves_the_search_field_behind() {
        let lattice = shelf_of("", &["/m/a.mp3"], 1);
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        assert!(cursor.navigate(Action::Up, &lattice));
        assert!(cursor
            .current_entry(&lattice)
            .and_then(Entry::search)
            .is_some());

        cursor.point_at_category(0, &lattice);
        assert!(
            cursor
                .current_entry(&lattice)
                .and_then(Entry::search)
                .is_none(),
            "the cursor is back on the category's own column"
        );
    }

    /// A column long enough that an `f32` cannot hold a thousandth of a row
    /// still *finishes* moving.
    ///
    /// It did not. The spring arrived one representable step short of its
    /// target, could not move again, and kept a velocity it could never shed —
    /// so the bar reported itself still travelling on every frame for the rest
    /// of the session, and the shell drew sixty of them a second over a
    /// distance smaller than a pixel. A shelf of the user's own photographs is
    /// exactly the column long enough to reach it.
    #[test]
    fn a_cursor_a_long_way_down_a_column_stops_moving() {
        let rows: Vec<Entry> = (0..8_000).map(|i| entry(&format!("row{i}"))).collect();
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: rows,
            }],
            OsString::from("lxb-test"),
        );

        for row in [1_024, 4_097, 7_999] {
            let mut cursor = cursor(&lattice);
            assert!(cursor.point_at_row(row, &lattice));
            // A second is far longer than any of these takes to arrive; what
            // is being asserted is that it ever says so.
            let mut frames = 0;
            while cursor.animate(1.0 / 60.0) {
                frames += 1;
                assert!(frames < 600, "still moving at row {row} after ten seconds");
            }
            assert_eq!(cursor.selected_item(), row);
        }
    }

    /// The bar can grow a column mid-session — a machine with music on it but
    /// no media player installed has no Multimedia column until the walk finds
    /// a file. Every display is standing in that bar at the time.
    #[test]
    fn a_column_appearing_does_not_move_the_one_the_user_is_on() {
        let lattice = model();
        let mut cursor = cursor(&lattice);
        assert!(cursor.navigate(Action::Right, &lattice));
        assert_eq!(cursor.selected_category, 1);
        while cursor.animate(1.0 / 60.0) {}
        let settled = cursor.category_position;

        // One arrives in front of where they are standing.
        cursor.category_added(1);
        assert_eq!(cursor.selected_category, 2, "the same column, moved along");
        assert_eq!(cursor.category_position, settled + 1.0);
        assert_eq!(cursor.selected_items.len(), lattice.categories.len() + 1);

        // And one behind it moves nothing at all.
        cursor.category_added(3);
        assert_eq!(cursor.selected_category, 2);
        assert_eq!(cursor.category_position, settled + 1.0);
    }

    /// A bar of named columns, each with three rows in it.
    fn bar(ids: &[&'static str]) -> Lattice {
        Lattice::with_wayland_display(
            ids.iter()
                .map(|id| Category {
                    id,
                    title: "column",
                    icon: "column",
                    entries: vec![entry("one"), entry("two"), entry("three")],
                })
                .collect(),
            OsString::from("lxb-test"),
        )
    }

    /// The whole bar can be read off the disk again under a user who is
    /// standing in it — turning the Steam integration off takes one column away
    /// and puts a row back in another — and the cursor keeps its place by name.
    #[test]
    fn a_bar_rebuilt_underneath_a_cursor_keeps_its_place_by_name() {
        // Standing on the third row of Games, with a row remembered in Steam.
        let held = bar(&["settings", "steam", "games"]);
        let mut cursor = cursor(&held);
        cursor.selected_category = 1;
        assert!(cursor.point_at_row(2, &held));
        cursor.selected_category = 2;
        cursor.category_position = 2.0;
        assert!(cursor.point_at_row(1, &held));
        let footing = cursor.remember(&held);

        // Steam goes. Games is where it was, under a smaller number.
        let now = bar(&["settings", "games"]);
        cursor.recall(&footing, &now);
        assert_eq!(cursor.selected_category, 1, "the same column by name");
        assert_eq!(cursor.category_position, 1.0, "placed, not travelled");
        assert_eq!(cursor.selected_item(), 1, "and the same row of it");
        assert_eq!(cursor.selected_items.len(), now.categories.len());

        // And what Steam had been left on is not inherited by whoever took its
        // number, which is the whole reason this is done by name.
        cursor.selected_category = 0;
        assert_eq!(cursor.selected_item(), 0, "Settings was never stood in");
    }

    /// The column somebody was standing in can be the one that goes. They land
    /// on the one that closed the gap, at the top of it, which is what a paper
    /// list does when a line is struck out.
    #[test]
    fn a_cursor_in_the_column_that_went_lands_on_the_one_that_replaced_it() {
        let held = bar(&["settings", "steam", "games"]);
        let mut cursor = cursor(&held);
        cursor.selected_category = 1;
        cursor.category_position = 1.0;
        assert!(cursor.point_at_row(2, &held));
        let footing = cursor.remember(&held);

        let now = bar(&["settings", "games"]);
        cursor.recall(&footing, &now);
        assert_eq!(cursor.selected_category, 1, "Games closed the gap");
        assert_eq!(cursor.selected_item(), 0, "at the top of it");
        assert_eq!(cursor.depth(), 0, "and out of any path into the old one");
    }

    /// The bug this pair was written for, and the reason a rebuild used to look
    /// like the shell restarting: the columns the shell hangs back on the bar
    /// itself — the Steam library, the trophies, RetroArch — are added *after*
    /// the scan, under a cursor that is still holding the old bar's numbers.
    /// Every one of them walks that number a column further along, and what
    /// used to be put back was whatever the walked-past number happened to
    /// name.
    #[test]
    fn a_column_hung_back_on_during_a_rebuild_does_not_move_the_cursor() {
        // Each column anybody could be standing in when something is installed,
        // and the row of it they were on.
        for standing in 0..4 {
            let held = bar(&["games", "steam", "video", "settings"]);
            let mut cursor = cursor(&held);
            cursor.selected_category = standing;
            cursor.category_position = standing as f32;
            assert!(cursor.point_at_row(2, &held));
            let footing = cursor.remember(&held);

            // The scan finds what is on the disk, which is every column but the
            // library — that one is the shell's own and is hung back on
            // afterwards, under a cursor still holding the old bar's numbers.
            let scanned = bar(&["games", "video", "settings"]);
            cursor.keep_in_bounds(&scanned);
            let now = bar(&["games", "steam", "video", "settings"]);
            cursor.category_added(1);

            cursor.recall(&footing, &now);
            assert_eq!(
                cursor.selected_category, standing,
                "still in the column they were standing in"
            );
            assert_eq!(cursor.selected_item(), 2, "on the row they were on");
        }
    }

    /// And the path they had opened is still open. A rebuild is not a Back
    /// press: somebody three levels down the Settings tree when a package lands
    /// is still three levels down it afterwards.
    #[test]
    fn a_rebuild_does_not_close_the_path_a_cursor_had_opened() {
        let held = nested();
        let mut cursor = cursor(&held);
        cursor.navigate(Action::Down, &held);
        assert!(cursor.enter(&held), "there is a subcategory to step into");
        let depth = cursor.depth();
        assert_eq!(depth, 1);
        let footing = cursor.remember(&held);

        // The rebuild throws the path away, the way `shelve_games` does on its
        // way past — and recalling the footing is what puts it back.
        cursor.leave_subcolumns();
        let now = nested();
        cursor.recall(&footing, &now);
        assert_eq!(cursor.depth(), depth, "the same path, still open");
    }

    /// A bar rebuilt mid-stride goes on walking. Something installing is not a
    /// press, and a bar that arrived early because a package landed would be
    /// the shell snatching the move out of somebody's thumb.
    #[test]
    fn a_rebuild_does_not_cut_a_move_short() {
        let held = bar(&["games", "video", "settings"]);
        let mut cursor = cursor(&held);
        cursor.selected_category = 1;
        cursor.category_position = 0.4;
        cursor.category_speed = 3.0;
        let footing = cursor.remember(&held);

        let now = bar(&["games", "steam", "video", "settings"]);
        cursor.recall(&footing, &now);
        assert_eq!(cursor.selected_category, 2, "Video, one column along");
        assert_eq!(
            cursor.category_position, 1.4,
            "as far from it as it was, so the glide carries on"
        );
        assert_eq!(cursor.category_speed, 3.0, "and at the speed it was going");
    }

    /// The trap this avoids: the shell's own Settings column is subcategories
    /// all the way down, so a Right that opened one there would be a Right
    /// that could never reach the column after it.
    #[test]
    fn the_column_after_a_column_of_subcategories_is_still_reachable() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.current_entry(&lattice).unwrap().entries().is_some());

        assert!(cursor.navigate(Action::Right, &lattice));
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);
    }

    /// A list of values opens on the value in force. The answer to "what is
    /// this set to" should be where the cursor already is rather than
    /// something to go looking for, which is how the original's settings lists
    /// behaved and the one thing a list of one entry can still get wrong.
    #[test]
    fn a_list_of_values_opens_on_the_one_in_force() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.enter(&lattice);
        cursor.navigate(Action::Right, &lattice);

        assert_eq!(cursor.selected_item(), 1, "Purple, not the top of the list");
        assert!(cursor.current_entry(&lattice).is_some_and(Entry::chosen));
    }

    /// Choosing a value moves the mark onto it and takes it off the row that
    /// had it. The two have to happen together: a column showing two values in
    /// force, or none, is a column that has stopped answering the question it
    /// was opened to answer.
    #[test]
    fn choosing_a_value_moves_the_mark_within_its_column() {
        let mut lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.enter(&lattice);
        cursor.navigate(Action::Right, &lattice);
        assert_eq!(
            cursor.selected_item(),
            1,
            "opened on Purple, the one in force"
        );

        cursor.navigate(Action::Up, &lattice);
        assert_eq!(cursor.choose(&mut lattice), Some(Setting::Accent("Green")));

        let values = cursor.current_entries(&lattice);
        assert!(values[0].chosen(), "Green");
        assert!(!values[1].chosen(), "Purple, no longer");
        assert_eq!(values.iter().filter(|entry| entry.chosen()).count(), 1);

        // And again, back the other way: nothing about this is one-shot.
        cursor.navigate(Action::Down, &lattice);
        assert_eq!(cursor.choose(&mut lattice), Some(Setting::Accent("Purple")));
        assert!(cursor.current_entries(&lattice)[1].chosen());
    }

    /// Only a value that stands for something is chosen. A row that starts a
    /// process, a row that opens a column, and a value the shell can show but
    /// not change are all left exactly as they were — the last of those
    /// especially: taking the mark off a reading that is true, to put it on a
    /// row that changes nothing, would make the column lie.
    #[test]
    fn a_row_that_sets_nothing_is_not_chosen() {
        let mut lattice = nested();
        let mut cursor = cursor(&lattice);
        assert_eq!(cursor.choose(&mut lattice), None, "an application");

        cursor.navigate(Action::Down, &lattice);
        assert_eq!(cursor.choose(&mut lattice), None, "a subcategory");
        assert!(
            cursor.current_entry(&lattice).unwrap().entries().is_some(),
            "and it is still a subcategory"
        );

        let mut readings = Lattice::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: vec![reading("Connected", true), reading("Offline", false)],
            }],
            OsString::from("lxb-test"),
        );
        let mut on_reading = Cursor::new(readings.categories.len());
        on_reading.navigate(Action::Down, &readings);
        assert_eq!(on_reading.choose(&mut readings), None);
        assert!(
            on_reading.current_entries(&readings)[0].chosen(),
            "still Connected"
        );
        assert!(!on_reading.current_entries(&readings)[1].chosen());
    }

    /// A subcategory can be one of a set of answers as well as a way further
    /// in, and where it is, the column opens on it exactly as it opens on a
    /// chosen value.
    ///
    /// The wireless network a radio is on is the one row in the tree like this.
    /// A Networks column that opened on its first row would open on "Not
    /// connected" while the machine was connected — the list saying the
    /// opposite of what is true, on the frame it arrives.
    #[test]
    fn a_column_opens_on_a_chosen_subcategory() {
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: vec![folder(
                    "Networks",
                    vec![
                        choice("Not connected", false),
                        chosen_folder("Upstairs", vec![choice("Automatic", true)]),
                        choice("The Cafe", false),
                    ],
                )],
            }],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        assert_eq!(cursor.selected_item(), 1, "the network it is on");
        assert!(cursor.current_entry(&lattice).is_some_and(Entry::chosen));
        // And it is still a way in.
        assert!(cursor.enter(&lattice));
        assert_eq!(titles(cursor.current_entries(&lattice)), ["Automatic"]);
    }

    /// A column rewritten under a standing cursor cannot leave it pointing past
    /// the end of the list.
    ///
    /// This is not a hypothetical. Clearing the DNS servers on a connection
    /// hands the question back to the network, and the row the field was opened
    /// from stops existing on the same frame — leaving a column drawn scrolled
    /// off its own bottom, nothing under the highlight, and no way back to the
    /// list but Up, pressed once per row that vanished.
    #[test]
    fn a_column_that_loses_rows_keeps_the_cursor_on_one() {
        let page = |rows: Vec<Entry>| {
            Lattice::with_wayland_display(
                vec![Category {
                    id: "a",
                    title: "A",
                    icon: "a",
                    entries: vec![folder("DNS", rows)],
                }],
                OsString::from("lxb-test"),
            )
        };
        let lattice = page(vec![
            choice("Automatic", false),
            choice("Manual", true),
            reading("DNS servers", false),
        ]);
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        cursor.navigate(Action::Down, &lattice);
        cursor.navigate(Action::Down, &lattice);
        assert_eq!(cursor.selected_item(), 2);

        // The field is cleared, so the row it was on is no longer part of the
        // question and the answer in force has moved.
        let lattice = page(vec![choice("Automatic", true), choice("Manual", false)]);
        assert!(cursor.keep_in_bounds(&lattice));
        assert_eq!(
            cursor.selected_item(),
            0,
            "the value in force, which is the answer that is now true"
        );
        assert!(!cursor.keep_in_bounds(&lattice), "and nothing moves twice");

        // With no value in force to fall back on, the last row that exists.
        let lattice = page(vec![
            choice("Automatic", false),
            choice("Manual", false),
            reading("DNS servers", false),
        ]);
        let mut standing = Cursor::new(lattice.categories.len());
        assert!(standing.enter(&lattice));
        standing.navigate(Action::Down, &lattice);
        standing.navigate(Action::Down, &lattice);
        let lattice = page(vec![choice("Automatic", false)]);
        assert!(standing.keep_in_bounds(&lattice));
        assert_eq!(standing.selected_item(), 0);
    }

    /// A row that acts takes no mark and moves none.
    ///
    /// The two halves matter separately. A tick on Forget would be the shell
    /// claiming a press is a state; a tick taken *off* Automatic because the
    /// user pressed Forget beside it would be the column telling a lie about
    /// something else entirely. See [`crate::apps::Choice::acts`].
    #[test]
    fn a_row_that_acts_takes_no_mark_and_moves_none() {
        let mut lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: vec![folder(
                    "Upstairs",
                    vec![choice("Automatic", true), acting("Forget")],
                )],
            }],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        cursor.navigate(Action::Down, &lattice);

        assert_eq!(cursor.choose(&mut lattice), Some(Setting::Accent("Forget")));
        let rows = cursor.current_entries(&lattice);
        assert!(!rows[1].chosen(), "a press is not a state to be in");
        assert!(
            rows[0].chosen(),
            "and the value in force is still the value in force"
        );

        // The row beside it is an ordinary answer and still behaves like one.
        cursor.navigate(Action::Up, &lattice);
        assert_eq!(
            cursor.choose(&mut lattice),
            Some(Setting::Accent("Automatic"))
        );
        assert!(cursor.current_entries(&lattice)[0].chosen());
    }

    /// A cursor the *shell* moves never comes to rest on a row that acts.
    ///
    /// The case is real and one press away: Disconnect under a wireless
    /// network turns a four-row page into a two-row one, and a cursor that
    /// clamped to the end of what was left would put the user's thumb on
    /// Forget with no press of their own in between.
    #[test]
    fn a_cursor_put_back_in_bounds_lands_short_of_a_row_that_acts() {
        let page = |rows: Vec<Entry>| {
            Lattice::with_wayland_display(
                vec![Category {
                    id: "a",
                    title: "A",
                    icon: "a",
                    entries: vec![folder("Upstairs", rows)],
                }],
                OsString::from("lxb-test"),
            )
        };
        let lattice = page(vec![
            folder("IP address", vec![entry("x")]),
            folder("DNS", vec![entry("x")]),
            acting("Disconnect"),
            acting("Forget"),
        ]);
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        for _ in 0..2 {
            cursor.navigate(Action::Down, &lattice);
        }
        assert_eq!(cursor.selected_item(), 2, "on Disconnect");

        // Pressed. The network is off, and the page is the shorter one a
        // network the radio is not on gets.
        let lattice = page(vec![acting("Connect"), acting("Forget")]);
        assert!(cursor.keep_in_bounds(&lattice));
        assert_eq!(
            cursor.selected_item(),
            0,
            "the first row, because every row here acts and the top of a column \
             is where this tree puts the one that does least"
        );

        // Where there is a row that does not act, the last of those.
        let lattice = page(vec![
            folder("IP address", vec![entry("x")]),
            acting("Connect"),
            acting("Forget"),
        ]);
        let mut standing = Cursor::new(lattice.categories.len());
        assert!(standing.enter(&lattice));
        for _ in 0..2 {
            standing.navigate(Action::Down, &lattice);
        }
        let lattice = page(vec![
            folder("IP address", vec![entry("x")]),
            acting("Forget"),
        ]);
        assert!(standing.keep_in_bounds(&lattice));
        assert_eq!(standing.selected_item(), 0);
    }

    /// A column that empties altogether is stepped out of. There is no row to
    /// put the cursor on, and a column with nothing in it is the one shape the
    /// bar cannot show — [`Cursor::enter`] refuses to open one, and a cursor
    /// already standing in one has to be got out the same way.
    #[test]
    fn a_column_that_empties_is_stepped_out_of() {
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: vec![folder("Wi-Fi", vec![folder("Networks", vec![entry("x")])])],
            }],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&lattice);
        assert!(cursor.enter(&lattice));
        assert!(cursor.enter(&lattice));
        assert_eq!(cursor.depth(), 2);

        // The radio is switched off: the networks go, and so does the column
        // they were in.
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: vec![folder("Wi-Fi", vec![folder("Networks", Vec::new())])],
            }],
            OsString::from("lxb-test"),
        );
        assert!(cursor.keep_in_bounds(&lattice));
        assert_eq!(cursor.depth(), 1, "back out to the page it hung from");
        assert_eq!(titles(cursor.current_entries(&lattice)), ["Networks"]);

        // And when the row it hung from goes too, out again — as far as there
        // is still a column to stand in.
        let lattice = Lattice::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: vec![folder("Wi-Fi", Vec::new())],
            }],
            OsString::from("lxb-test"),
        );
        assert!(cursor.keep_in_bounds(&lattice));
        assert_eq!(cursor.depth(), 0);
        assert_eq!(titles(cursor.current_entries(&lattice)), ["Wi-Fi"]);
    }

    /// The trail is checked too, not only the column the cursor is standing in.
    /// A subcategory that falls off the end of the column it hangs from would
    /// otherwise take the whole path with it — the cursor left pointing at a
    /// row that is not there, through a row that is not there either.
    #[test]
    fn the_whole_path_is_brought_back_inside_the_tree() {
        let page = |sockets: Vec<Entry>| {
            Lattice::with_wayland_display(
                vec![Category {
                    id: "a",
                    title: "A",
                    icon: "a",
                    entries: sockets,
                }],
                OsString::from("lxb-test"),
            )
        };
        let lattice = page(vec![
            folder("test-wired0", vec![choice("Off", false)]),
            folder(
                "test-wired1",
                vec![choice("Off", false), choice("On", true)],
            ),
        ]);
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.enter(&lattice));
        assert_eq!(cursor.selected_item(), 1, "opened on the value in force");

        // The second socket is unplugged and drops out of the listing.
        let lattice = page(vec![folder("test-wired0", vec![choice("Off", false)])]);
        assert!(cursor.keep_in_bounds(&lattice));
        assert_eq!(titles(cursor.current_entries(&lattice)), ["Off"]);
        assert_eq!(cursor.selected_item(), 0);
    }

    /// Neither a subcategory nor a value starts a process, and the shell asks
    /// this before it puts a splash up: a folder that answered with the first
    /// application inside it would launch something nobody chose.
    #[test]
    fn only_an_application_row_is_launchable() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        assert!(cursor.current_app(&lattice).is_some());

        cursor.navigate(Action::Down, &lattice);
        assert!(cursor.current_app(&lattice).is_none(), "a subcategory");
        assert_eq!(cursor.current_setting(&lattice), None);
        cursor.enter(&lattice);
        cursor.navigate(Action::Right, &lattice);
        assert!(cursor.current_app(&lattice).is_none(), "a value");
        assert_eq!(
            cursor.current_setting(&lattice),
            Some(Setting::Accent("Purple"))
        );
    }

    /// A column stepped out of has to be watchable leaving — it is still on
    /// screen while the bar slides back off it — and stepping straight back in
    /// returns to the row it was left on, because it is the same column and
    /// nothing has moved it.
    #[test]
    fn the_column_stepped_out_of_is_still_there_to_leave() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.enter(&lattice);
        cursor.navigate(Action::Right, &lattice);
        cursor.navigate(Action::Up, &lattice);
        settle(&mut cursor);
        assert_eq!(cursor.selected_item(), 0, "Green");

        cursor.navigate(Action::Left, &lattice);
        assert_eq!(
            cursor.columns(&lattice).len(),
            3,
            "the column being left is still drawn while it slides away"
        );

        cursor.navigate(Action::Right, &lattice);
        assert_eq!(
            cursor.selected_item(),
            0,
            "stepping back in returns to the row it was left on"
        );

        // Moving off the row it hangs from is what finally drops it: it is no
        // longer a column anything on screen leads to.
        cursor.navigate(Action::Left, &lattice);
        cursor.navigate(Action::Left, &lattice);
        assert_eq!(cursor.depth(), 0);
        cursor.navigate(Action::Up, &lattice);
        assert_eq!(cursor.columns(&lattice).len(), 1);
    }

    /// Changing category leaves the whole path behind — the columns belong to
    /// a tree the bar is no longer in — but the depth is left to ease back
    /// rather than snapped, so the cross returns from wherever it had slid to.
    #[test]
    fn a_path_does_not_survive_a_change_of_category() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.enter(&lattice);
        settle(&mut cursor);
        assert_eq!(cursor.depth_position(), 1.0);

        cursor.navigate(Action::Left, &lattice);
        assert!(
            cursor.navigate(Action::Right, &lattice),
            "now the row moves"
        );
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);
        assert_eq!(cursor.columns(&lattice).len(), 1);
        assert!(
            cursor.depth_position() > 0.0,
            "the cross should travel back rather than jump"
        );

        settle(&mut cursor);
        assert_eq!(cursor.depth_position(), 0.0);
    }

    /// Up and Down belong to the column the user is standing in, and its ends
    /// are that column's — not the category's.
    #[test]
    fn the_open_column_is_the_one_that_scrolls() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.enter(&lattice);

        // Accent colour is the only row in Appearance.
        assert!(!cursor.navigate(Action::Down, &lattice));
        assert!(!cursor.navigate(Action::Up, &lattice));

        cursor.navigate(Action::Right, &lattice);
        assert!(
            cursor.navigate(Action::Up, &lattice),
            "two values to move between"
        );
        assert_eq!(cursor.selected_item(), 0);
        assert!(!cursor.navigate(Action::Up, &lattice));
        assert!(cursor.navigate(Action::Down, &lattice));
        assert!(!cursor.navigate(Action::Down, &lattice));
    }

    /// The step in is a move of the bar, so it is sprung like every other one:
    /// it travels, it never overshoots, and it stops.
    /// A column stepped back into opens on a row that is still there.
    ///
    /// The remembered row is a *number*, and the rows it counted are rewritten
    /// by everything that touches the catalogue. This is the bug a ROM folder
    /// showed on the day it was chosen: the picker is a walk several columns
    /// deep, answering it rebuilds the column it was opened from, and the first
    /// console stepped into afterwards opened on a row left over from somebody
    /// else's listing — a column of games with no game under the highlight and
    /// its only row drawn off the top of the screen.
    #[test]
    fn stepping_back_into_a_column_lands_on_a_row_that_exists() {
        let lattice = Lattice::new(vec![Category {
            id: "retroarch",
            title: "RetroArch",
            icon: "r",
            entries: vec![folder("PlayStation Portable", vec![entry("T6 EU")])],
        }]);
        let mut cursor = Cursor::new(lattice.categories.len());

        // Where a walk four rows deep left it — the shape `leave` leaves
        // behind, with the row numbers of a listing that has since gone.
        assert!(cursor.enter(&lattice));
        cursor.stack[0].selected = 4;
        cursor.stack[0].position = 4.0;
        assert!(cursor.leave());

        assert!(cursor.settle(&lattice), "the remembered row is not there");
        assert!(cursor.enter(&lattice), "back into the console");
        assert_eq!(cursor.selected_item(), 0);
        assert_eq!(
            cursor.position_at(1),
            0.0,
            "and the column is not drawn scrolled off its own top"
        );

        // And a walk that is not somewhere to go back to at all.
        assert!(cursor.leave());
        cursor.stack[0].selected = 4;
        cursor.forget_the_way_back();
        assert!(cursor.enter(&lattice));
        assert_eq!(cursor.selected_item(), 0);
    }

    /// A row answered and then gone leaves the cursor somewhere that exists.
    ///
    /// The BIOS row is the case: it is the last row of the RetroArch column, it
    /// asks a question, and answering it is what takes it off the column. So
    /// the row somebody was standing on stops existing *because* they pressed
    /// it, and the number remembered for that category points past the end —
    /// a highlight over nothing, above a list drawn scrolled off its own top.
    ///
    /// The stack half of [`Cursor::settle`] has always covered the subcolumns.
    /// The category's own column is held somewhere else and was not.
    #[test]
    fn a_cursor_on_a_row_that_goes_away_is_put_on_one_that_has_not() {
        let full = Lattice::new(vec![Category {
            id: "retroarch",
            title: "RetroArch",
            icon: "r",
            entries: vec![
                entry("PlayStation 2"),
                entry("PlayStation Portable"),
                entry("PlayStation 2 BIOS"),
            ],
        }]);
        let mut cursor = Cursor::new(full.categories.len());
        assert!(cursor.point_at_row(2, &full));
        assert_eq!(cursor.selected_item(), 2);

        // The firmware is put in place, the folder is read again, and the row
        // that asked for it is not there any more.
        let fewer = Lattice::new(vec![Category {
            id: "retroarch",
            title: "RetroArch",
            icon: "r",
            entries: vec![entry("PlayStation 2"), entry("PlayStation Portable")],
        }]);
        assert!(cursor.settle(&fewer), "it had to move");
        assert_eq!(cursor.selected_item(), 1, "onto the last row there is");
        assert_eq!(
            cursor.position_at(0),
            1.0,
            "and the column is not drawn scrolled off its own top"
        );
        assert!(!cursor.settle(&fewer), "and it stays where it was put");
    }

    /// Being sent to a column means the column, not the third subcolumn of it
    /// somebody is standing in.
    ///
    /// The bug: a game that cannot start without a BIOS raises a panel offering
    /// to go and find one, and the row that asks is on the RetroArch column —
    /// two columns back out from the game whose press raised the panel.
    /// [`Cursor::select_category`] answers a *walk* along the category row, and
    /// a walk that arrives where it started is not a move, so it returned
    /// having done nothing and the offer pointed at a row of the column the
    /// cursor was already deep inside. Pressing it did nothing at all.
    #[test]
    fn a_cursor_sent_to_a_column_comes_out_to_it() {
        let lattice = Lattice::new(vec![Category {
            id: "retroarch",
            title: "RetroArch",
            icon: "r",
            entries: vec![
                folder("PlayStation 2", vec![entry("Tekken Tag")]),
                entry("PlayStation 2 BIOS"),
            ],
        }]);
        let mut cursor = Cursor::new(lattice.categories.len());

        // Standing on the game, two columns in.
        assert!(cursor.enter(&lattice));
        assert_eq!(cursor.depth(), 1);

        cursor.go_to_own_column(0, &lattice);
        assert_eq!(cursor.depth(), 0, "back out to the column itself");
        assert!(
            cursor.point_at_row(1, &lattice),
            "and the row it was sent to is one this column has"
        );
        assert_eq!(
            cursor.current_entry(&lattice).map(Entry::title),
            Some("PlayStation 2 BIOS")
        );
    }

    /// A path the catalogue stops holding is one the cursor comes back out of.
    ///
    /// This is the black screen: the folder picker hangs off a row of the
    /// RetroArch column, and answering it rebuilds that column — so the columns
    /// the cursor was standing in stopped existing while it stood in them. The
    /// bar draws one step to the left per level opened, so what was on screen
    /// afterwards was the wallpaper and nothing else, with a shell running
    /// behind it.
    #[test]
    fn a_cursor_does_not_stand_deeper_than_the_bar_goes() {
        let deep = |leaf: Vec<Entry>| {
            Lattice::new(vec![Category {
                id: "retroarch",
                title: "RetroArch",
                icon: "r",
                entries: vec![folder("Games folder", leaf)],
            }])
        };
        let walked = deep(vec![entry("ROMs")]);
        let mut cursor = Cursor::new(walked.categories.len());
        assert!(cursor.enter(&walked));
        assert_eq!(cursor.depth(), 1);
        assert!(!cursor.settle(&walked), "nothing has gone anywhere");

        // The listing the cursor was standing in, rebuilt out from under it.
        let rebuilt = deep(Vec::new());
        assert!(cursor.settle(&rebuilt), "the column it was in has gone");
        assert_eq!(cursor.depth(), 0);
        assert!(
            !cursor.current_entries(&rebuilt).is_empty(),
            "and it is on the bar"
        );

        // And a column that goes altogether, not merely its rows.
        let mut cursor = Cursor::new(walked.categories.len());
        assert!(cursor.enter(&walked));
        let gone = Lattice::new(vec![Category {
            id: "retroarch",
            title: "RetroArch",
            icon: "r",
            entries: vec![entry("PlayStation Portable")],
        }]);
        assert!(cursor.settle(&gone));
        assert_eq!(cursor.depth(), 0);
    }

    #[test]
    fn depth_travels_and_settles_without_overshooting() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.enter(&lattice);
        assert_eq!(cursor.depth_position(), 0.0, "it starts where it was");

        let mut previous = 0.0;
        let mut frames = 0;
        while cursor.animate(1.0 / 60.0) {
            let at = cursor.depth_position();
            assert!(
                at > previous,
                "the step should advance: {at} after {previous}"
            );
            assert!(at <= 1.0, "it must not overshoot: {at}");
            previous = at;
            frames += 1;
            assert!(frames < 600, "the step never settled");
        }
        assert_eq!(cursor.depth_position(), 1.0);

        // Measured against a step along the category row, which is the only
        // motion on screen to compare it with. The journey is the wider of the
        // two, so it takes longer — but a step that took half again as long
        // would stop reading as the same interface moving.
        let mut sideways = self::cursor(&lattice);
        sideways.navigate(Action::Right, &lattice);
        let mut row_frames = 0;
        while sideways.animate(1.0 / 60.0) {
            row_frames += 1;
        }
        assert!(
            (row_frames..row_frames * 3 / 2).contains(&frames),
            "a step in took {frames} frames against the row's {row_frames}"
        );
    }

    /// A column keeps easing while the user is a level away from it: one that
    /// froze the moment it stopped being the column in front would slide off
    /// screen mid-glide, with its rows caught between rows.
    #[test]
    fn a_column_left_mid_glide_finishes_its_glide() {
        let lattice = nested();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Down, &lattice);
        cursor.enter(&lattice);
        cursor.navigate(Action::Right, &lattice);
        cursor.navigate(Action::Up, &lattice);

        // Away before the row it just left has arrived anywhere.
        cursor.animate(1.0 / 60.0);
        let column = &cursor.columns(&lattice)[2];
        assert!(
            column.position > 0.0 && column.position < 1.0,
            "the value list should be mid-glide: {}",
            column.position
        );

        cursor.navigate(Action::Left, &lattice);
        settle(&mut cursor);
        assert_eq!(cursor.columns(&lattice)[2].position, 0.0);
    }

    #[test]
    fn displays_browse_independently() {
        // The whole point of a cursor per display: moving one must not drag
        // the others along with it.
        let lattice = model();
        let mut left = cursor(&lattice);
        let mut right = cursor(&lattice);

        left.navigate(Action::Down, &lattice);
        left.navigate(Action::Right, &lattice);

        assert_eq!(left.selected_category, 1);
        assert_eq!(
            right.selected_category, 0,
            "the other display must not move"
        );
        assert_eq!(right.selected_item(), 0);

        right.navigate(Action::Down, &lattice);
        assert_eq!(right.selected_item(), 1);
        assert_eq!(
            left.selected_category, 1,
            "and moving that one must not drag the first back"
        );
    }

    #[test]
    fn animation_converges_and_stops() {
        let lattice = model();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Right, &lattice);

        // Sixty frames at 60fps is a second, far longer than the ease takes.
        let mut frames = 0;
        while cursor.animate(1.0 / 60.0) {
            frames += 1;
            assert!(frames < 600, "animation never settled");
        }

        assert!(!cursor.animate(1.0 / 60.0), "animation should stay settled");
        assert_eq!(cursor.category_position, 1.0);
        assert_eq!(cursor.item_position, 0.0);
    }

    #[test]
    fn animation_is_monotonic_towards_the_target() {
        let lattice = model();
        let mut cursor = cursor(&lattice);
        cursor.navigate(Action::Right, &lattice);

        let mut previous = cursor.category_position;
        for _ in 0..10 {
            cursor.animate(1.0 / 60.0);
            assert!(
                cursor.category_position > previous,
                "position should advance towards the target"
            );
            assert!(cursor.category_position <= 1.0, "must not overshoot");
            previous = cursor.category_position;
        }
    }

    #[test]
    fn empty_model_is_inert() {
        let lattice = Lattice::with_wayland_display(Vec::new(), OsString::from("lxb-test"));
        let mut cursor = cursor(&lattice);
        assert!(lattice.is_empty());
        assert!(!cursor.navigate(Action::Right, &lattice));
        assert!(cursor.current_app(&lattice).is_none());
    }

    /// A catalogue with nothing launchable in it is empty however many rows it
    /// has — the shell's own Settings column is always there and holds nothing
    /// that starts a process, so counting rows would call it stocked.
    #[test]
    fn a_model_with_nothing_to_launch_is_empty() {
        let column = |id: &'static str, entries: Vec<Entry>| Category {
            id,
            title: id,
            icon: id,
            entries,
        };
        let settings = column(
            "settings",
            vec![folder("Appearance", vec![choice("Purple", true)])],
        );
        let lattice = Lattice::with_wayland_display(vec![settings], OsString::from("lxb-test"));
        assert!(lattice.is_empty());

        let mut stocked = lattice;
        stocked.categories.push(column(
            "games",
            vec![folder("Emulators", vec![entry("a1")])],
        ));
        assert!(
            !stocked.is_empty(),
            "an application inside a subcategory is still one to launch"
        );
    }

    /// The shell's own column leads the bar and is full of rows that launch
    /// nothing, so "somewhere with something in it" has to mean somewhere with
    /// an *application* in it. Opening every session on Settings would be a
    /// poor greeting however many rows it has.
    #[test]
    fn a_display_opens_on_a_column_with_something_to_launch() {
        let mut lattice = model();
        lattice.categories.insert(
            0,
            Category {
                id: "settings",
                title: "Settings",
                icon: "preferences-system",
                entries: vec![folder("Appearance", vec![choice("Purple", true)])],
            },
        );

        let cursor = Cursor::for_model(&lattice);
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(
            cursor.category_position, 1.0,
            "it should start there rather than slide there"
        );

        // With nothing anywhere there is no better column to prefer, and the
        // cursor must still be valid.
        let bare = Lattice::with_wayland_display(Vec::new(), OsString::from("lxb-test"));
        assert_eq!(Cursor::for_model(&bare).selected_category, 0);
    }

    /// A session opens on the column the user asked for, and falls back to the
    /// first column with something in it when that column is not on this bar.
    ///
    /// The fallback is not a nicety. A column named by the setting comes and
    /// goes with an account or a package — Steam, RetroArch, Waydroid — and a
    /// shell that insisted on an index would open on whichever column had taken
    /// that place.
    #[test]
    fn a_display_opens_where_the_settings_say() {
        let mut lattice = model();
        lattice.categories.insert(
            0,
            Category {
                id: "settings",
                title: "Settings",
                icon: "preferences-system",
                entries: vec![folder("Appearance", vec![choice("Purple", true)])],
            },
        );
        let last = lattice.categories.len() - 1;
        let named = lattice.categories[last].id;

        crate::settings::note_startup_category(Some(named));
        let cursor = Cursor::for_model(&lattice);
        assert_eq!(cursor.selected_category, last);
        assert_eq!(
            cursor.category_position, last as f32,
            "it should start there rather than slide there"
        );

        // Even where that column has nothing to launch. Somebody who chose to
        // open on Settings meant it, and Settings never has anything to launch.
        crate::settings::note_startup_category(Some("settings"));
        assert_eq!(Cursor::for_model(&lattice).selected_category, 0);

        // And a column this bar has not got falls back to what the shell did
        // before there was a setting at all.
        crate::settings::note_startup_category(Some("no-such-column"));
        assert_eq!(Cursor::for_model(&lattice).selected_category, 1);

        crate::settings::note_startup_category(None);
    }

    fn argv_strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter()
            .map(|arg| arg.into_string().expect("test argv should be UTF-8"))
            .collect()
    }

    #[test]
    fn terminal_environment_takes_priority_and_preserves_options() {
        let argv = terminal_argv_with(
            Some("konsole --separate --profile 'Game Tools'"),
            "printf '%s\\n' ready",
            |program| matches!(program.to_str(), Some("konsole" | "xdg-terminal-exec")),
        )
        .unwrap();

        assert_eq!(
            argv_strings(argv),
            [
                "konsole",
                "--separate",
                "--profile",
                "Game Tools",
                "-e",
                "sh",
                "-c",
                "printf '%s\\n' ready",
            ]
        );
    }

    #[test]
    fn malformed_or_missing_terminal_environment_uses_available_fallback() {
        let argv = terminal_argv_with(Some("kitty 'unterminated"), "htop", |program| {
            matches!(program.to_str(), Some("konsole" | "xterm"))
        })
        .unwrap();

        assert_eq!(
            argv_strings(argv),
            ["konsole", "--separate", "-e", "sh", "-c", "htop"]
        );
    }

    #[test]
    fn standard_terminal_resolver_precedes_named_fallbacks() {
        let argv = terminal_argv_with(Some("missing-terminal"), "top", |program| {
            matches!(
                program.to_str(),
                Some("xdg-terminal-exec" | "konsole" | "xterm")
            )
        })
        .unwrap();

        assert_eq!(
            argv_strings(argv),
            ["xdg-terminal-exec", "--", "sh", "-c", "top"]
        );
    }

    #[test]
    fn terminal_families_get_their_required_exec_separator() {
        let cases = [
            ("foot", vec!["foot", "-e", "sh", "-c", "cmd"]),
            ("ghostty", vec!["ghostty", "-e", "sh", "-c", "cmd"]),
            ("kitty", vec!["kitty", "--", "sh", "-c", "cmd"]),
            (
                "gnome-terminal",
                vec!["gnome-terminal", "--", "sh", "-c", "cmd"],
            ),
            (
                "xfce4-terminal",
                vec!["xfce4-terminal", "-x", "sh", "-c", "cmd"],
            ),
            ("wezterm", vec!["wezterm", "start", "--", "sh", "-c", "cmd"]),
        ];

        for (terminal, expected) in cases {
            let argv = terminal_argv_with(Some(terminal), "cmd", |program| {
                program == OsStr::new(terminal)
            })
            .unwrap();
            assert_eq!(argv_strings(argv), expected, "terminal={terminal}");
        }
    }

    #[test]
    fn configured_exec_separator_is_not_duplicated_and_konsole_is_isolated() {
        let argv = terminal_argv_with(Some("konsole -e"), "cmd", |program| {
            program == OsStr::new("konsole")
        })
        .unwrap();

        assert_eq!(
            argv_strings(argv),
            ["konsole", "--separate", "-e", "sh", "-c", "cmd"]
        );
    }

    #[test]
    fn terminal_resolution_fails_cleanly_when_none_are_installed() {
        assert!(terminal_argv_with(None, "htop", |_| false).is_none());
    }

    #[test]
    fn launch_exposes_the_real_command_exit_status() {
        let mut child = launch(
            "exit 23",
            false,
            Pads::ForAnApplication,
            OsStr::new("lxb-test"),
            None,
        )
        .expect("the shell command should spawn");

        let status = child.wait().expect("the command should be waitable");
        assert_eq!(status.code(), Some(23));
    }

    #[test]
    fn model_reaps_finished_launches() {
        let mut lattice = model();
        match &mut lattice.categories[0].entries[0] {
            Entry::App(app) => app.exec = "exit 17".into(),
            other => panic!("the first row should be an application: {other:?}"),
        }
        let cursor = cursor(&lattice);

        assert!(lattice.launch_selected(&cursor).is_some());
        assert_eq!(lattice.launched_apps.len(), 1);

        let deadline = Instant::now() + Duration::from_secs(2);
        while !lattice.launched_apps.is_empty() && Instant::now() < deadline {
            lattice.reap_children();
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(
            lattice.launched_apps.is_empty(),
            "finished application should be reaped"
        );
    }

    /// A game that comes straight back is reported; anything else is only
    /// tidied away.
    ///
    /// The whole of what tells this shell a game did not start. Before it, an
    /// emulator that exited half a second after the press left the screen back
    /// on the bar with nothing anywhere saying why, and the only record was a
    /// line in a log nobody on a sofa is reading.
    #[test]
    fn a_game_that_would_not_start_is_reported_and_a_program_is_not() {
        let played = |lattice: &mut Lattice, exit: &str| {
            lattice.play_rom(&crate::apps::Rom {
                name: "Ridge Racer".to_string(),
                path: PathBuf::from("/roms/ps1/Ridge Racer.chd"),
                console: "PlayStation".to_string(),
                note: "PlayStation".to_string(),
                wanted: vec!["mednafen_psx".to_string()],
                start: Some(vec!["sh".to_string(), "-c".to_string(), exit.to_string()]),
                boxart: None,
                snap: None,
                own_cover: false,
                own_background: false,
                shape: None,
                glyph: "lxb:console-ps1".to_string(),
            })
        };
        let reap = |lattice: &mut Lattice| {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut failed = Vec::new();
            while !lattice.launched_apps.is_empty() && Instant::now() < deadline {
                failed.extend(lattice.reap_children());
                std::thread::sleep(Duration::from_millis(5));
            }
            failed
        };

        let mut lattice = model();
        assert!(played(&mut lattice, "exit 1").is_some());
        let failed = reap(&mut lattice);
        assert_eq!(failed.len(), 1, "the emulator came straight back");
        assert_eq!(failed[0].console, "PlayStation");
        assert_eq!(failed[0].rom, PathBuf::from("/roms/ps1/Ridge Racer.chd"));
        assert!(
            failed[0].lasted < Duration::from_secs(5),
            "and how long it lasted is what says it was a start rather than a crash"
        );

        // A game somebody played and quit is not a failure.
        assert!(played(&mut lattice, "exit 0").is_some());
        assert!(reap(&mut lattice).is_empty(), "it ran and it ended");

        // Nor is anything that is not a game: a program that will not start is
        // its own business, and this shell has nothing to offer it.
        match &mut lattice.categories[0].entries[0] {
            Entry::App(app) => app.exec = "exit 1".into(),
            other => panic!("the first row should be an application: {other:?}"),
        }
        let cursor = cursor(&lattice);
        assert!(lattice.launch_selected(&cursor).is_some());
        assert!(reap(&mut lattice).is_empty(), "not a game, not answered");
    }

    #[cfg(unix)]
    #[test]
    fn launched_apps_are_pinned_to_the_shell_wayland_socket() {
        let mut command = Command::new("true");
        command
            .env("WAYLAND_DISPLAY", "host-wayland")
            .env("WAYLAND_SOCKET", "23")
            .env("DISPLAY", ":1");

        confine_to_session(
            &mut command,
            Pads::ForAnApplication,
            OsStr::new("lxb-test"),
            None,
        );

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(get("WAYLAND_DISPLAY"), Some(Some(OsStr::new("lxb-test"))));
        assert_eq!(get("WAYLAND_SOCKET"), Some(None));
        assert_eq!(get("DISPLAY"), Some(None));
        assert_eq!(get("LXB_XWAYLAND_DISPLAY"), Some(None));
    }

    #[test]
    fn launched_x11_apps_receive_only_lxb_private_display() {
        let mut command = Command::new("true");
        command
            .env("DISPLAY", ":0")
            .env("LXB_XWAYLAND_DISPLAY", ":0");

        confine_to_session(
            &mut command,
            Pads::ForAnApplication,
            OsStr::new("lxb-test"),
            Some(OsStr::new(":62")),
        );

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(get("DISPLAY"), Some(Some(OsStr::new(":62"))));
        assert_eq!(get("LXB_XWAYLAND_DISPLAY"), Some(Some(OsStr::new(":62"))));
    }
}
