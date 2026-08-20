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
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

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
    sort: crate::media::Sort,
) -> Option<crate::media::Orders> {
    let Some(Entry::Folder(folder)) = entries.get_mut(row) else {
        return None;
    };
    let place = folder.place.clone()?;
    let shown = match place {
        crate::files::Place::Volumes => crate::files::volumes(query),
        crate::files::Place::Directory(at) => crate::files::listing(&at, query, sort),
    };
    folder.entries = shown.rows;
    // What was found, in place of the date the row was carrying: "14 folders,
    // 6 files" is what the user has just been shown, and a row that went on
    // saying when the folder was last written would be answering a question
    // nobody asked twice. What a *search* has left of it is on the field
    // instead, which is the row that is doing the narrowing.
    folder.comment = Some(shown.note);
    Some(shown.orders)
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
        let app = cursor.current_app(self)?;
        let name = app.name.clone();
        let entry = app.path.clone();
        let command = app.exec.clone();
        let terminal = app.terminal;
        tracing::info!(app = %name, entry = %entry.display(), "launching");

        match launch(
            &command,
            terminal,
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
                });
                Some(pid)
            }
            Err(err) => {
                tracing::warn!(app = %name, command, ?err, "failed to start application");
                None
            }
        }
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
        let opening = crate::media::opening(&file.path, file.mime, &self.categories)?;
        self.open_media_with(file, opening)
    }

    /// The same, for a file the explorer found in a folder.
    ///
    /// Its own two lines rather than a shared one, because the two rows carry
    /// different things and the difference is the whole of what the user
    /// sees: a shelved song is titled without its extension and this is titled
    /// with it, which is what the column it was pressed in is *for*.
    fn open_file(&mut self, file: &crate::files::Item) -> Option<u32> {
        let opening = crate::media::opening(&file.path, file.mime, &self.categories)?;
        self.open_path_with(&file.path, &file.name, opening)
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
        self.open_path_with(&file.path, &file.title, opening)
    }

    /// What both of those come down to: start `opening`, and file the process
    /// under the name of the file rather than the name of the program.
    fn open_path_with(
        &mut self,
        path: &Path,
        title: &str,
        opening: crate::media::Opening,
    ) -> Option<u32> {
        tracing::info!(
            file = %path.display(),
            with = %opening.name,
            "opening a file the shell found"
        );

        match launch(
            &opening.command,
            false,
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
                });
                Some(pid)
            }
            Err(err) => {
                tracing::warn!(command = %opening.command, ?err, "failed to start a player");
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
    pub fn reap_children(&mut self) {
        let now = Instant::now();
        self.launched_apps
            .retain_mut(|launched| match launched.child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt;
                    let runtime_ms = now.duration_since(launched.started_at).as_millis();
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
                        (false, None) => tracing::warn!(
                            app = %launched.name,
                            command = %launched.command,
                            %status,
                            runtime_ms,
                            "application process exited unsuccessfully"
                        ),
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

    /// A cursor for a display just coming up, resting on the first column that
    /// has anything in it.
    ///
    /// The shell's own Settings column leads the bar and has nothing under it
    /// yet, and opening every session onto an empty column would be a poor
    /// greeting. It is placed rather than travelled to, so the bar is already
    /// where it belongs on the first frame.
    pub fn for_model(lattice: &Lattice) -> Self {
        let mut cursor = Self::new(lattice.categories.len());
        if let Some(populated) = lattice.categories.iter().position(Category::has_launchable) {
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
        sort: crate::media::Sort,
    ) -> Option<crate::media::Orders> {
        let row = self.selected_item();
        let entries = self.level_entries_mut(lattice, self.open)?;
        // A fresh visit, so the field it opens with is empty. A search belongs
        // to the looking somebody is doing rather than to the folder — walking
        // out of one and into another is a different question, and a column
        // that arrived already narrowed by what was typed in the last one would
        // be hiding files with no field in sight to say so.
        let orders = read_into(entries, row, "", sort)?;

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
        sort: crate::media::Sort,
    ) -> Option<crate::media::Orders> {
        let level = self.open.checked_sub(1)?;
        let row = self.row_at(level);
        let entries = self.level_entries_mut(lattice, level)?;
        read_into(entries, row, query, sort)
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
        let Some(was) = self.row_in_column(at) else {
            return;
        };
        let Some(entries) = lattice.categories.get(at).map(|column| &column.entries) else {
            return;
        };
        let Some(row) = entries
            .iter()
            .position(|entry| entry.game().is_some_and(|game| game.app_id == app_id))
        else {
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
        if self.open == 0 {
            return;
        }
        let was = self.selected_item();
        let Some(row) = self
            .current_entries(lattice)
            .iter()
            .position(|entry| entry.game().is_some_and(|game| game.app_id == app_id))
        else {
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

/// Start an application in an independent process session.
///
/// LineXinBar retains the returned handle only to collect its exit status. A
/// [`Child`] handle does not own the process lifetime: if the shell exits first,
/// the kernel reparents the running process and it continues normally.
pub fn launch(
    command: &str,
    terminal: bool,
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

    confine_to_session(&mut cmd, wayland_display, xwayland_display);

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
    if let Some(mut argv) = configured.and_then(split_terminal_command) {
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

/// Minimal POSIX-style splitting for `$TERMINAL`, including quoted arguments.
/// An unterminated quote or escape rejects the value and triggers fallback.
fn split_terminal_command(input: &str) -> Option<Vec<String>> {
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

    hide_guarded_pads_from_hidapi(command);
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
    let Some(guarded) = crate::pad_guard::hidapi_ignore_list() else {
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
            title: title.into(),
            comment: None,
            icon: None,
            entries,
            place: None,
            chosen: false,
            over_the_list: false,
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
            chosen,
            acts: false,
            setting: Some(Setting::Accent(title)),
        })
    }

    /// A row that does a thing rather than being one of a set of answers.
    fn acting(title: &'static str) -> Entry {
        Entry::Choice(crate::apps::Choice {
            title: title.into(),
            comment: None,
            icon: None,
            swatch: None,
            chosen: false,
            acts: true,
            setting: Some(Setting::Accent(title)),
        })
    }

    /// A value the shell can show but not change.
    fn reading(title: &str, chosen: bool) -> Entry {
        Entry::Choice(crate::apps::Choice {
            title: title.into(),
            comment: None,
            icon: None,
            swatch: None,
            chosen,
            acts: false,
            setting: None,
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
                                "Accent color",
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

    /// The order every folder opens in until somebody chooses another.
    fn by_name() -> crate::media::Sort {
        crate::media::Sort::NameAscending
    }

    /// One column of two rows, both of them somewhere on the disk: the bar as
    /// it stands the moment somebody has stepped into Files.
    fn places(first: &Path, second: &Path) -> Lattice {
        let place = |title: &str, at: &Path| {
            Entry::Folder(crate::apps::Folder {
                title: title.into(),
                comment: None,
                icon: None,
                entries: Vec::new(),
                place: Some(crate::files::Place::Directory(at.to_path_buf())),
                chosen: false,
                over_the_list: false,
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
            ["Search", "inside", "a.txt"],
            "the field stands over the folder"
        );
        assert_eq!(
            cursor.selected_item(),
            1,
            "and the column opens on the folder, not on the field"
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
        assert_eq!(cursor.selected_item(), 3);

        assert!(cursor.leave(), "back out to the folder it came from");
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);
        assert_eq!(
            cursor.selected_item(),
            1,
            "the first row of the listing, under the field that stands over it"
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
        assert_eq!(rows, ["alp", "Clear search", "alpha.txt"]);

        // And out again, without stepping anywhere: the row that empties the
        // field is the only thing that undoes one.
        assert!(cursor.search_here(&mut lattice, "", by_name()).is_some());
        assert_eq!(
            cursor.current_entries(&lattice).len(),
            4,
            "the field, and three"
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
        assert_eq!(cursor.current_entries(&lattice).len(), 3);

        assert!(cursor.leave());
        cursor.open_place(&mut lattice, by_name());
        cursor.enter(&lattice);
        assert_eq!(
            cursor.current_entries(&lattice).len(),
            3,
            "the field, and both files"
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
            Some("Accent color")
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
            Some("Accent color")
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
            app_id,
            name: name.into(),
            note: String::new(),
            installed,
            updating: false,
            steam_client: true,
        })
    }

    /// The row a Steam library carries over its first game.
    fn index(games: Vec<Entry>) -> Entry {
        Entry::Folder(crate::apps::Folder {
            title: "Alphabetical".into(),
            comment: None,
            icon: None,
            entries: games,
            place: None,
            chosen: false,
            over_the_list: true,
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

        // Accent color is the only row in Appearance.
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
        let mut child = launch("exit 23", false, OsStr::new("lxb-test"), None)
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

    #[cfg(unix)]
    #[test]
    fn launched_apps_are_pinned_to_the_shell_wayland_socket() {
        let mut command = Command::new("true");
        command
            .env("WAYLAND_DISPLAY", "host-wayland")
            .env("WAYLAND_SOCKET", "23")
            .env("DISPLAY", ":1");

        confine_to_session(&mut command, OsStr::new("lxb-test"), None);

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
