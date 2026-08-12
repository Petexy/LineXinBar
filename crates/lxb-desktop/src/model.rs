//! The XMB navigation model.
//!
//! The cross media bar is two orthogonal lists: categories run horizontally,
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
}

/// The applications available to launch, and the processes started from them.
///
/// Shared by every display: what each one is *pointing at* lives in its own
/// [`Cursor`], but there is only one catalogue and one set of running children.
pub struct Xmb {
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
    /// where you were, as the real XMB does.
    selected_items: Vec<usize>,

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

impl Xmb {
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
        let opening = crate::media::opening(file, &self.categories)?;
        self.open_media_with(file, opening)
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
        tracing::info!(
            file = %file.path.display(),
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
                    name: file.title.clone(),
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
            selected_items: vec![0; categories],
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
    pub fn for_model(xmb: &Xmb) -> Self {
        let mut cursor = Self::new(xmb.categories.len());
        if let Some(populated) = xmb.categories.iter().position(Category::has_launchable) {
            cursor.selected_category = populated;
            cursor.category_position = populated as f32;
        }
        cursor
    }

    /// The row the cursor is on, in whichever column it is standing in.
    pub fn selected_item(&self) -> usize {
        match self.open.checked_sub(1) {
            None => self
                .selected_items
                .get(self.selected_category)
                .copied()
                .unwrap_or(0),
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

    pub fn current_category<'a>(&self, xmb: &'a Xmb) -> Option<&'a Category> {
        xmb.categories.get(self.selected_category)
    }

    /// The rows of the column the cursor is standing in.
    pub fn current_entries<'a>(&self, xmb: &'a Xmb) -> &'a [Entry] {
        self.level_entries(xmb, self.open).unwrap_or_default()
    }

    /// The row the cursor is on.
    pub fn current_entry<'a>(&self, xmb: &'a Xmb) -> Option<&'a Entry> {
        self.current_entries(xmb).get(self.selected_item())
    }

    /// What the cursor is on, when it is on something launchable. `None` for a
    /// subcategory or a setting, neither of which starts a process.
    pub fn current_app<'a>(&self, xmb: &'a Xmb) -> Option<&'a App> {
        self.current_entry(xmb)?.app()
    }

    /// What the highlighted value would apply, without moving the catalogue's
    /// chosen mark. The shell uses this to preview a setting while the cursor
    /// is merely standing on it.
    pub fn current_setting(&self, xmb: &Xmb) -> Option<Setting> {
        self.current_entry(xmb)?.setting()
    }

    /// Every column the bar is showing, outermost first: the category's own,
    /// then one per subcategory stepped into — and, for as long as it is still
    /// sliding away, the one just stepped out of.
    pub fn columns<'a>(&self, xmb: &'a Xmb) -> Vec<Column<'a>> {
        let mut out = Vec::with_capacity(self.stack.len() + 1);
        for level in 0..=self.stack.len() {
            let Some(entries) = self.level_entries(xmb, level) else {
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
    fn level_entries<'a>(&self, xmb: &'a Xmb, level: usize) -> Option<&'a [Entry]> {
        let mut entries = xmb
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
    fn level_entries_mut<'a>(&self, xmb: &'a mut Xmb, level: usize) -> Option<&'a mut [Entry]> {
        let mut entries = xmb
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
    pub fn choose(&self, xmb: &mut Xmb) -> Option<Setting> {
        let row = self.selected_item();
        let entries = self.level_entries_mut(xmb, self.open)?;
        let setting = match entries.get(row)? {
            Entry::Choice(choice) => choice.setting?,
            _ => return None,
        };
        for (index, entry) in entries.iter_mut().enumerate() {
            if let Entry::Choice(choice) = entry {
                choice.chosen = index == row;
            }
        }
        Some(setting)
    }

    /// The row selected in the column at `level`.
    fn row_at(&self, level: usize) -> usize {
        match level.checked_sub(1) {
            None => self
                .selected_items
                .get(self.selected_category)
                .copied()
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
                    *slot = row;
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
    pub fn point_at_row(&mut self, row: usize, xmb: &Xmb) -> bool {
        if row >= self.current_entries(xmb).len() || row == self.selected_item() {
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
    pub fn point_at_category(&mut self, index: usize, xmb: &Xmb) -> bool {
        if index >= xmb.categories.len() {
            return false;
        }
        if index == self.selected_category && self.open == 0 {
            return false;
        }
        self.selected_category = index;
        self.restore_column();
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
    pub fn keep_on_media(&mut self, xmb: &Xmb, file: &crate::media::Shelved) {
        let was = self.selected_item();
        let Some(row) = self
            .current_entries(xmb)
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

    /// Which game this cursor's row in the column at `at` is on, if it is on
    /// one. The pair of [`Self::keep_on_game`], asked before the column is
    /// rebuilt so there is something to keep it on afterwards.
    ///
    /// The column at `at` rather than the one the cursor is standing in: the
    /// row a cursor is on in a category it is *not* in is remembered all the
    /// same, and walking back to a library that re-sorted while the user was
    /// elsewhere would land them on a different game for exactly the same
    /// reason.
    pub fn game_in_column(&self, xmb: &Xmb, at: usize) -> Option<u32> {
        let row = *self.selected_items.get(at)?;
        Some(xmb.categories.get(at)?.entries.get(row)?.game()?.app_id)
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
    pub fn keep_on_game(&mut self, xmb: &Xmb, at: usize, app_id: u32) {
        let Some(was) = self.selected_items.get(at).copied() else {
            return;
        };
        let Some(entries) = xmb.categories.get(at).map(|column| &column.entries) else {
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
        self.selected_items[at] = row;
        // Only the column that is on screen has a drawn position to move, and
        // for a category it is always the outermost one: a game is the end of a
        // path, so there is never a subcolumn open over the top of this.
        if self.selected_category == at {
            self.item_position += row as f32 - was as f32;
        }
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
    pub fn rest_on_first_row(&mut self, xmb: &Xmb) {
        let row = first_row(self.current_entries(xmb));
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
        self.selected_items.insert(at, 0);
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
    pub fn category_removed(&mut self, at: usize, xmb: &Xmb) {
        if at >= self.selected_items.len() {
            return;
        }
        self.selected_items.remove(at);

        let last = xmb.categories.len().saturating_sub(1);
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
            self.rest_on_first_row(xmb);
        }
    }

    /// Go to one column by name, from wherever the cursor is.
    ///
    /// Travelled rather than placed, unlike everything above: this is a move
    /// the *user* asked for — pressing the Steam row to be taken to their
    /// library — and the whole point of the bar sliding is that they can see
    /// where they were taken.
    pub fn select_category(&mut self, at: usize, xmb: &Xmb) {
        if at >= xmb.categories.len() || at == self.selected_category {
            return;
        }
        self.leave_subcolumns();
        self.selected_category = at;
        self.restore_column();
    }

    /// Come back out of every subcategory this cursor is standing in.
    fn leave_subcolumns(&mut self) {
        self.open = 0;
        self.stack.clear();
    }

    /// Step into the subcategory under the cursor. `false` if the row is not
    /// one — an application, or a setting, both of which are the end of a path.
    pub fn enter(&mut self, xmb: &Xmb) -> bool {
        let Some(entries) = self.current_entry(xmb).and_then(Entry::entries) else {
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
    pub fn navigate(&mut self, action: Action, xmb: &Xmb) -> bool {
        if xmb.categories.is_empty() {
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
                self.restore_column();
                true
            }
            Action::Right => {
                if self.open > 0 {
                    // Deeper, or nothing: inside a column the category row is
                    // not somewhere Right may jump to, because Left — the only
                    // way back — means something else in here.
                    return self.enter(xmb);
                }
                if self.selected_category + 1 >= xmb.categories.len() {
                    return false;
                }
                self.selected_category += 1;
                self.restore_column();
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
                let count = self.current_entries(xmb).len();
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
    fn restore_column(&mut self) {
        self.stack.clear();
        self.open = 0;
        self.item_position = self.selected_item() as f32;
        self.item_speed = 0.0;
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

/// Make an application use the same display servers as the shell.
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
        })
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
            setting: None,
        })
    }

    fn model() -> Xmb {
        Xmb::with_wayland_display(
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
    fn nested() -> Xmb {
        Xmb::with_wayland_display(
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

    fn cursor(xmb: &Xmb) -> Cursor {
        Cursor::new(xmb.categories.len())
    }

    /// A catalogue with one real application in it, filed inside a
    /// subcategory: nothing about a window says which column of the bar its
    /// application ended up in, so the lookup has to go all the way down.
    fn installed() -> Xmb {
        let mut browser = app("Firefox");
        browser.wm_class = Some("firefox".into());
        Xmb::with_wayland_display(
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
        let xmb = installed();

        assert_eq!(
            xmb.app_for_window("firefox").map(|app| app.name.as_str()),
            Some("Firefox")
        );
        // The two spellings the same application arrives under elsewhere in
        // the shell: an X11 class is capitalised, and a reverse-DNS id carries
        // the name in its tail.
        assert_eq!(
            xmb.app_for_window("Firefox").map(|app| app.name.as_str()),
            Some("Firefox")
        );
        assert_eq!(
            xmb.app_for_window("org.mozilla.firefox")
                .map(|app| app.name.as_str()),
            Some("Firefox")
        );

        // Nothing installed claims these, and neither may anything be invented
        // for them: the caller has its own answer for a window the catalogue
        // has never heard of.
        assert!(xmb.app_for_window("some-game").is_none());
        assert!(xmb.app_for_window("").is_none());
        assert!(xmb.app_for_window("   ").is_none());
    }

    fn settle(cursor: &mut Cursor) {
        while cursor.animate(1.0 / 60.0) {}
    }

    #[test]
    fn navigates_within_bounds() {
        let xmb = model();
        let mut cursor = cursor(&xmb);

        // Cannot move before the first category or above the first item.
        assert!(!cursor.navigate(Action::Left, &xmb));
        assert!(!cursor.navigate(Action::Up, &xmb));

        assert!(cursor.navigate(Action::Down, &xmb));
        assert_eq!(cursor.selected_item(), 1);
        assert!(cursor.navigate(Action::Right, &xmb));
        assert_eq!(cursor.selected_category, 1);

        // Category B has a single app, so Down does nothing.
        assert!(!cursor.navigate(Action::Down, &xmb));
        assert!(!cursor.navigate(Action::Right, &xmb));
    }

    /// A pointer names the row it wants outright, where a direction can only
    /// ask for the next one — and the bar still travels there, so the click
    /// reads as the column being scrolled rather than replaced.
    #[test]
    fn a_row_can_be_pointed_at_directly() {
        let xmb = model();
        let mut cursor = cursor(&xmb);

        assert!(cursor.point_at_row(2, &xmb));
        assert_eq!(cursor.selected_item(), 2);
        assert!(cursor.item_position < 2.0, "and travels there");

        // The row it is already on is not a move, which is what tells a second
        // click on a row apart from the first.
        assert!(!cursor.point_at_row(2, &xmb));
        // Nor is a row the column does not have.
        assert!(!cursor.point_at_row(9, &xmb));
        assert_eq!(cursor.selected_item(), 2);
    }

    /// Pointing at a category steps out of whatever path is open first, exactly
    /// as walking left to the row would: arriving with a path still open would
    /// leave a trail belonging to a category the user has left.
    #[test]
    fn pointing_at_a_category_leaves_the_path_behind() {
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        assert!(cursor.enter(&xmb));
        assert_eq!(cursor.depth(), 1);

        assert!(cursor.point_at_category(1, &xmb));
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);

        // The category it is already on, with nothing open, is not a move.
        assert!(!cursor.point_at_category(1, &xmb));
        assert!(!cursor.point_at_category(7, &xmb));
    }

    /// The one case where pointing at the category already selected *is* a
    /// move: the cursor is inside a path hanging off it, and the button at the
    /// head of that trail is the way back out.
    #[test]
    fn pointing_at_the_open_categorys_button_walks_back_out_to_it() {
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        assert!(cursor.enter(&xmb));

        assert!(cursor.point_at_category(cursor.selected_category, &xmb));
        assert_eq!(cursor.depth(), 0);
    }

    #[test]
    fn remembers_selection_per_category() {
        let xmb = model();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.navigate(Action::Down, &xmb);
        assert_eq!(cursor.selected_item(), 2);

        cursor.navigate(Action::Right, &xmb);
        assert_eq!(cursor.selected_item(), 0);

        cursor.navigate(Action::Left, &xmb);
        assert_eq!(cursor.selected_item(), 2, "selection should be restored");
    }

    /// Restored, not travelled to. A remembered row that the column scrolls
    /// down to on the way back is the bar claiming something moved while the
    /// user was in another category, which is not what happened.
    #[test]
    fn a_remembered_row_is_already_there_rather_than_scrolled_to() {
        let xmb = model();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.navigate(Action::Down, &xmb);
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.item_position, 2.0);

        // Away: the column belongs to the next category from the first frame.
        cursor.navigate(Action::Right, &xmb);
        assert_eq!(cursor.item_position, 0.0, "and not on its way there");

        // And back, with nothing left to animate vertically — only the
        // category row is still travelling.
        cursor.navigate(Action::Left, &xmb);
        assert_eq!(cursor.item_position, 2.0);
        cursor.animate(1.0 / 60.0);
        assert_eq!(cursor.item_position, 2.0, "it must not drift off the row");

        // Moving inside a category still glides: this is about crossing
        // between them, not about killing the bar's vertical easing.
        assert!(cursor.navigate(Action::Up, &xmb));
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
        let xmb = nested();
        let mut cursor = cursor(&xmb);

        // At the top of a column the horizontal axis still belongs to the
        // category row, whatever kind of row the cursor is on.
        cursor.navigate(Action::Down, &xmb);
        assert!(cursor.navigate(Action::Right, &xmb));
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);
        assert!(cursor.navigate(Action::Left, &xmb));

        // Opening is Launch's job there — and from then on the axis is depth.
        cursor.navigate(Action::Down, &xmb);
        assert!(cursor.enter(&xmb), "Appearance opens");
        assert_eq!(cursor.depth(), 1);
        assert_eq!(
            cursor.selected_category, 0,
            "opening a subcategory must not also move along the row"
        );
        assert_eq!(
            cursor.current_entry(&xmb).map(Entry::title),
            Some("Accent color")
        );

        assert!(cursor.navigate(Action::Right, &xmb));
        assert_eq!(cursor.depth(), 2);
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("Purple"));

        // A value is the end of the path: there is nothing further right, and
        // the category row is not reachable from inside a column.
        assert!(!cursor.navigate(Action::Right, &xmb));
        assert_eq!(cursor.selected_category, 0);

        // And back out, one level per press, to the row it was opened from.
        assert!(cursor.navigate(Action::Left, &xmb));
        assert_eq!(
            cursor.current_entry(&xmb).map(Entry::title),
            Some("Accent color")
        );
        assert!(cursor.navigate(Action::Left, &xmb));
        assert_eq!(
            cursor.current_entry(&xmb).map(Entry::title),
            Some("Appearance")
        );
        assert_eq!(cursor.depth(), 0);
        // Only now does Left mean the category row again.
        assert!(!cursor.navigate(Action::Left, &xmb));
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
        let xmb = Xmb::with_wayland_display(
            vec![Category {
                id: "multimedia",
                title: "Multimedia",
                icon: "multimedia",
                entries: vec![folder("Music", Vec::new()), entry("Audacity")],
            }],
            OsString::from("lxb-test"),
        );
        let mut cursor = cursor(&xmb);
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("Music"));

        assert!(
            !cursor.enter(&xmb),
            "there is nothing in there to step into"
        );
        assert_eq!(cursor.depth(), 0);
        assert_eq!(cursor.columns(&xmb).len(), 1);

        // And the row is still the one under the light, so the column the user
        // is looking at is the column they were looking at.
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("Music"));
        assert!(cursor.navigate(Action::Down, &xmb));
        assert_eq!(
            cursor.current_entry(&xmb).map(Entry::title),
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
            Xmb::with_wayland_display(
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
        let xmb = column(vec![song(&beta), song(&delta)]);
        let mut cursor = cursor(&xmb);
        assert!(cursor.enter(&xmb));
        assert!(cursor.navigate(Action::Down, &xmb));
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("delta"));
        while cursor.animate(1.0 / 60.0) {}
        let settled = cursor.position_at(1);

        // Two more turn up, one of them above the row being read.
        let xmb = column(vec![
            song(&alpha),
            song(&beta),
            song(&delta),
            song(&epsilon),
        ]);
        cursor.keep_on_media(&xmb, &delta);

        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("delta"));
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

    /// A library with these games in it, in this order.
    fn library(games: Vec<Entry>) -> Xmb {
        Xmb::with_wayland_display(
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
        let xmb = library(vec![
            game(1, "Aeonic", true),
            game(2, "Zenith", true),
            game(3, "Celeste", false),
            game(4, "Downloading", false),
        ]);
        let mut cursor = cursor(&xmb);
        for _ in 0..3 {
            assert!(cursor.navigate(Action::Down, &xmb));
        }
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.game_in_column(&xmb, 0), Some(4));
        let settled = cursor.position_at(0);

        // It lands, and the library comes back re-sorted around it.
        let xmb = library(vec![
            game(1, "Aeonic", true),
            game(4, "Downloading", true),
            game(2, "Zenith", true),
            game(3, "Celeste", false),
        ]);
        cursor.keep_on_game(&xmb, 0, 4);

        assert_eq!(cursor.selected_item(), 1, "two rows up the list");
        assert_eq!(cursor.game_in_column(&xmb, 0), Some(4));
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
            Xmb::with_wayland_display(categories, OsString::from("lxb-test"))
        };
        let xmb = games(false);
        let mut cursor = cursor(&xmb);
        cursor.select_category(1, &xmb);
        assert!(cursor.navigate(Action::Down, &xmb));
        cursor.select_category(0, &xmb);
        let elsewhere = cursor.position_at(0);

        let xmb = games(true);
        cursor.keep_on_game(&xmb, 1, 9);

        cursor.select_category(1, &xmb);
        assert_eq!(cursor.game_in_column(&xmb, 1), Some(9));
        assert_eq!(
            elsewhere,
            cursor.position_at(0),
            "and the column the display is actually in was not moved under it"
        );
    }

    /// A game that has left the library altogether — a shared title whose
    /// lender took it back — leaves the cursor where it is standing, the same
    /// answer a deleted file gets.
    #[test]
    fn a_game_that_leaves_the_library_leaves_the_cursor_where_it_stands() {
        let xmb = library(vec![game(1, "Aeonic", true), game(2, "Zenith", true)]);
        let mut cursor = cursor(&xmb);
        assert!(cursor.navigate(Action::Down, &xmb));

        let xmb = library(vec![game(1, "Aeonic", true), game(3, "Celeste", true)]);
        cursor.keep_on_game(&xmb, 0, 2);
        assert_eq!(cursor.selected_item(), 1);
        assert_eq!(cursor.game_in_column(&xmb, 0), Some(3));
    }

    /// A file that has gone off the disk takes its row with it, and the cursor
    /// stays where it is standing rather than following the file into nothing.
    #[test]
    fn a_song_deleted_from_under_the_cursor_leaves_it_where_it_stands() {
        let xmb = Xmb::with_wayland_display(
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
        let mut cursor = cursor(&xmb);
        assert!(cursor.enter(&xmb));
        assert!(cursor.navigate(Action::Down, &xmb));

        cursor.keep_on_media(&xmb, &shelved("/m/b.mp3"));
        assert_eq!(cursor.selected_item(), 1);
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("c"));
    }

    /// A shelf listed in a different order is a different list, so the cursor
    /// goes to the head of it — placed there, with nothing left travelling.
    #[test]
    fn a_reordered_column_is_shown_from_its_first_row() {
        let xmb = Xmb::with_wayland_display(
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
        let mut cursor = cursor(&xmb);
        assert!(cursor.enter(&xmb));
        assert!(cursor.navigate(Action::Down, &xmb));
        assert!(cursor.navigate(Action::Down, &xmb));
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.selected_item(), 2);

        cursor.rest_on_first_row(&xmb);
        assert_eq!(cursor.selected_item(), 0);
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("a"));
        // Placed, not travelled to: there is no journey through a list whose
        // every row has just changed.
        assert_eq!(cursor.position_at(1), 0.0);
        assert!(!cursor.animate(1.0 / 60.0), "nothing left to ease");
    }

    /// A shelf of the user's own files, as the worker hands it over: the search
    /// at the head of it, and then the files.
    /// `paths` is what the search has left; `found` is how many are on the
    /// shelf altogether, which is not the same number once one is running.
    fn shelf_of(query: &str, paths: &[&str], found: usize) -> Xmb {
        let listing: Vec<crate::media::Shelved> = paths.iter().map(|path| shelved(path)).collect();
        Xmb::with_wayland_display(
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
        let xmb = shelf_of("", &["/m/a.mp3", "/m/b.mp3"], 2);
        let mut cursor = cursor(&xmb);
        assert!(cursor.enter(&xmb));
        assert_eq!(cursor.selected_item(), 1);
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("a"));

        // And the field is exactly one press of Up away, which is where a
        // person looks for the thing above the first thing.
        assert!(cursor.navigate(Action::Up, &xmb));
        assert!(cursor.current_entry(&xmb).and_then(Entry::search).is_some());
        assert!(!cursor.navigate(Action::Up, &xmb), "and nothing above that");

        // Reordering the shelf brings the cursor back to the first file for the
        // same reason: what "newest first" asks to be shown is the newest file.
        cursor.navigate(Action::Down, &xmb);
        cursor.navigate(Action::Down, &xmb);
        while cursor.animate(1.0 / 60.0) {}
        cursor.rest_on_first_row(&xmb);
        assert_eq!(cursor.current_entry(&xmb).map(Entry::title), Some("a"));
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
        let xmb = shelf_of("zzz", &[], 40);
        let mut cursor = cursor(&xmb);
        assert!(cursor.enter(&xmb), "the two rows are still a column");
        assert_eq!(
            cursor.current_entry(&xmb).map(Entry::title),
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
        let xmb = shelf_of("", &["/m/a.mp3"], 1);
        let mut cursor = cursor(&xmb);
        assert!(cursor.enter(&xmb));
        assert!(cursor.navigate(Action::Up, &xmb));
        assert!(cursor.current_entry(&xmb).and_then(Entry::search).is_some());

        cursor.point_at_category(0, &xmb);
        assert!(
            cursor.current_entry(&xmb).and_then(Entry::search).is_none(),
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
        let xmb = Xmb::with_wayland_display(
            vec![Category {
                id: "a",
                title: "A",
                icon: "a",
                entries: rows,
            }],
            OsString::from("lxb-test"),
        );

        for row in [1_024, 4_097, 7_999] {
            let mut cursor = cursor(&xmb);
            assert!(cursor.point_at_row(row, &xmb));
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
        let xmb = model();
        let mut cursor = cursor(&xmb);
        assert!(cursor.navigate(Action::Right, &xmb));
        assert_eq!(cursor.selected_category, 1);
        while cursor.animate(1.0 / 60.0) {}
        let settled = cursor.category_position;

        // One arrives in front of where they are standing.
        cursor.category_added(1);
        assert_eq!(cursor.selected_category, 2, "the same column, moved along");
        assert_eq!(cursor.category_position, settled + 1.0);
        assert_eq!(cursor.selected_items.len(), xmb.categories.len() + 1);

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
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        assert!(cursor.current_entry(&xmb).unwrap().entries().is_some());

        assert!(cursor.navigate(Action::Right, &xmb));
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);
    }

    /// A list of values opens on the value in force. The answer to "what is
    /// this set to" should be where the cursor already is rather than
    /// something to go looking for, which is how the original's settings lists
    /// behaved and the one thing a list of one entry can still get wrong.
    #[test]
    fn a_list_of_values_opens_on_the_one_in_force() {
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);
        cursor.navigate(Action::Right, &xmb);

        assert_eq!(cursor.selected_item(), 1, "Purple, not the top of the list");
        assert!(cursor.current_entry(&xmb).is_some_and(Entry::chosen));
    }

    /// Choosing a value moves the mark onto it and takes it off the row that
    /// had it. The two have to happen together: a column showing two values in
    /// force, or none, is a column that has stopped answering the question it
    /// was opened to answer.
    #[test]
    fn choosing_a_value_moves_the_mark_within_its_column() {
        let mut xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);
        cursor.navigate(Action::Right, &xmb);
        assert_eq!(
            cursor.selected_item(),
            1,
            "opened on Purple, the one in force"
        );

        cursor.navigate(Action::Up, &xmb);
        assert_eq!(cursor.choose(&mut xmb), Some(Setting::Accent("Green")));

        let values = cursor.current_entries(&xmb);
        assert!(values[0].chosen(), "Green");
        assert!(!values[1].chosen(), "Purple, no longer");
        assert_eq!(values.iter().filter(|entry| entry.chosen()).count(), 1);

        // And again, back the other way: nothing about this is one-shot.
        cursor.navigate(Action::Down, &xmb);
        assert_eq!(cursor.choose(&mut xmb), Some(Setting::Accent("Purple")));
        assert!(cursor.current_entries(&xmb)[1].chosen());
    }

    /// Only a value that stands for something is chosen. A row that starts a
    /// process, a row that opens a column, and a value the shell can show but
    /// not change are all left exactly as they were — the last of those
    /// especially: taking the mark off a reading that is true, to put it on a
    /// row that changes nothing, would make the column lie.
    #[test]
    fn a_row_that_sets_nothing_is_not_chosen() {
        let mut xmb = nested();
        let mut cursor = cursor(&xmb);
        assert_eq!(cursor.choose(&mut xmb), None, "an application");

        cursor.navigate(Action::Down, &xmb);
        assert_eq!(cursor.choose(&mut xmb), None, "a subcategory");
        assert!(
            cursor.current_entry(&xmb).unwrap().entries().is_some(),
            "and it is still a subcategory"
        );

        let mut readings = Xmb::with_wayland_display(
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

    /// Neither a subcategory nor a value starts a process, and the shell asks
    /// this before it puts a splash up: a folder that answered with the first
    /// application inside it would launch something nobody chose.
    #[test]
    fn only_an_application_row_is_launchable() {
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        assert!(cursor.current_app(&xmb).is_some());

        cursor.navigate(Action::Down, &xmb);
        assert!(cursor.current_app(&xmb).is_none(), "a subcategory");
        assert_eq!(cursor.current_setting(&xmb), None);
        cursor.enter(&xmb);
        cursor.navigate(Action::Right, &xmb);
        assert!(cursor.current_app(&xmb).is_none(), "a value");
        assert_eq!(
            cursor.current_setting(&xmb),
            Some(Setting::Accent("Purple"))
        );
    }

    /// A column stepped out of has to be watchable leaving — it is still on
    /// screen while the bar slides back off it — and stepping straight back in
    /// returns to the row it was left on, because it is the same column and
    /// nothing has moved it.
    #[test]
    fn the_column_stepped_out_of_is_still_there_to_leave() {
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);
        cursor.navigate(Action::Right, &xmb);
        cursor.navigate(Action::Up, &xmb);
        settle(&mut cursor);
        assert_eq!(cursor.selected_item(), 0, "Green");

        cursor.navigate(Action::Left, &xmb);
        assert_eq!(
            cursor.columns(&xmb).len(),
            3,
            "the column being left is still drawn while it slides away"
        );

        cursor.navigate(Action::Right, &xmb);
        assert_eq!(
            cursor.selected_item(),
            0,
            "stepping back in returns to the row it was left on"
        );

        // Moving off the row it hangs from is what finally drops it: it is no
        // longer a column anything on screen leads to.
        cursor.navigate(Action::Left, &xmb);
        cursor.navigate(Action::Left, &xmb);
        assert_eq!(cursor.depth(), 0);
        cursor.navigate(Action::Up, &xmb);
        assert_eq!(cursor.columns(&xmb).len(), 1);
    }

    /// Changing category leaves the whole path behind — the columns belong to
    /// a tree the bar is no longer in — but the depth is left to ease back
    /// rather than snapped, so the cross returns from wherever it had slid to.
    #[test]
    fn a_path_does_not_survive_a_change_of_category() {
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);
        settle(&mut cursor);
        assert_eq!(cursor.depth_position(), 1.0);

        cursor.navigate(Action::Left, &xmb);
        assert!(cursor.navigate(Action::Right, &xmb), "now the row moves");
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(cursor.depth(), 0);
        assert_eq!(cursor.columns(&xmb).len(), 1);
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
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);

        // Accent color is the only row in Appearance.
        assert!(!cursor.navigate(Action::Down, &xmb));
        assert!(!cursor.navigate(Action::Up, &xmb));

        cursor.navigate(Action::Right, &xmb);
        assert!(
            cursor.navigate(Action::Up, &xmb),
            "two values to move between"
        );
        assert_eq!(cursor.selected_item(), 0);
        assert!(!cursor.navigate(Action::Up, &xmb));
        assert!(cursor.navigate(Action::Down, &xmb));
        assert!(!cursor.navigate(Action::Down, &xmb));
    }

    /// The step in is a move of the bar, so it is sprung like every other one:
    /// it travels, it never overshoots, and it stops.
    #[test]
    fn depth_travels_and_settles_without_overshooting() {
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);
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
        let mut sideways = self::cursor(&xmb);
        sideways.navigate(Action::Right, &xmb);
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
        let xmb = nested();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Down, &xmb);
        cursor.enter(&xmb);
        cursor.navigate(Action::Right, &xmb);
        cursor.navigate(Action::Up, &xmb);

        // Away before the row it just left has arrived anywhere.
        cursor.animate(1.0 / 60.0);
        let column = &cursor.columns(&xmb)[2];
        assert!(
            column.position > 0.0 && column.position < 1.0,
            "the value list should be mid-glide: {}",
            column.position
        );

        cursor.navigate(Action::Left, &xmb);
        settle(&mut cursor);
        assert_eq!(cursor.columns(&xmb)[2].position, 0.0);
    }

    #[test]
    fn displays_browse_independently() {
        // The whole point of a cursor per display: moving one must not drag
        // the others along with it.
        let xmb = model();
        let mut left = cursor(&xmb);
        let mut right = cursor(&xmb);

        left.navigate(Action::Down, &xmb);
        left.navigate(Action::Right, &xmb);

        assert_eq!(left.selected_category, 1);
        assert_eq!(
            right.selected_category, 0,
            "the other display must not move"
        );
        assert_eq!(right.selected_item(), 0);

        right.navigate(Action::Down, &xmb);
        assert_eq!(right.selected_item(), 1);
        assert_eq!(
            left.selected_category, 1,
            "and moving that one must not drag the first back"
        );
    }

    #[test]
    fn animation_converges_and_stops() {
        let xmb = model();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Right, &xmb);

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
        let xmb = model();
        let mut cursor = cursor(&xmb);
        cursor.navigate(Action::Right, &xmb);

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
        let xmb = Xmb::with_wayland_display(Vec::new(), OsString::from("lxb-test"));
        let mut cursor = cursor(&xmb);
        assert!(xmb.is_empty());
        assert!(!cursor.navigate(Action::Right, &xmb));
        assert!(cursor.current_app(&xmb).is_none());
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
        let xmb = Xmb::with_wayland_display(vec![settings], OsString::from("lxb-test"));
        assert!(xmb.is_empty());

        let mut stocked = xmb;
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
        let mut xmb = model();
        xmb.categories.insert(
            0,
            Category {
                id: "settings",
                title: "Settings",
                icon: "preferences-system",
                entries: vec![folder("Appearance", vec![choice("Purple", true)])],
            },
        );

        let cursor = Cursor::for_model(&xmb);
        assert_eq!(cursor.selected_category, 1);
        assert_eq!(
            cursor.category_position, 1.0,
            "it should start there rather than slide there"
        );

        // With nothing anywhere there is no better column to prefer, and the
        // cursor must still be valid.
        let bare = Xmb::with_wayland_display(Vec::new(), OsString::from("lxb-test"));
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
        let mut xmb = model();
        match &mut xmb.categories[0].entries[0] {
            Entry::App(app) => app.exec = "exit 17".into(),
            other => panic!("the first row should be an application: {other:?}"),
        }
        let cursor = cursor(&xmb);

        assert!(xmb.launch_selected(&cursor).is_some());
        assert_eq!(xmb.launched_apps.len(), 1);

        let deadline = Instant::now() + Duration::from_secs(2);
        while !xmb.launched_apps.is_empty() && Instant::now() < deadline {
            xmb.reap_children();
            std::thread::sleep(Duration::from_millis(5));
        }

        assert!(
            xmb.launched_apps.is_empty(),
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
