//! The XMB navigation model.
//!
//! The cross media bar is two orthogonal lists: categories run horizontally,
//! and the selected category's items run vertically through it. Selection is
//! integer, but the drawn position is a float that eases towards it, which is
//! what gives the interface its characteristic glide.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use crate::apps::{App, Category};

/// How stiff the spring the bar rides is, in radians per second. Higher is
/// snappier. Critically damped like the cards, so a move leans in and settles
/// rather than bolting off the mark, and holding a direction carries the
/// bar's momentum through each step instead of restarting it.
const EASE_RATE: f32 = 19.0;

/// Below this distance the animation is finished and we stop redrawing.
const SETTLED: f32 = 0.001;

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
    /// Hand control to the previous / next display.
    PrevScreen,
    NextScreen,
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
    /// Linboard's private XWayland display, when the compositor supplied one.
    /// An arbitrary inherited `DISPLAY` is never trusted here.
    xwayland_display: Option<OsString>,
    /// Direct children retained only so their exit status can be collected.
    /// Dropping a [`Child`] never terminates the process, so applications still
    /// outlive the shell if Linboard itself exits first.
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

    /// Eased position of the category bar, in category units.
    pub category_position: f32,
    /// Eased position of the item column, in item units.
    pub item_position: f32,
    /// How fast each of those is moving, in units per second. Held because a
    /// spring needs it: it is what makes the bar lean into a move instead of
    /// leaving at full speed, and what carries the momentum of a held
    /// direction from one step into the next rather than starting over.
    category_speed: f32,
    item_speed: f32,
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

    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }

    /// Start whatever `cursor` is pointing at. Returns the process id of what
    /// it started — `None` if there was nothing there, or if it would not
    /// start, which the caller needs to tell apart from a slow launch before
    /// it puts a splash up over one that is never coming.
    pub fn launch_selected(&mut self, cursor: &Cursor) -> Option<u32> {
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

    /// Name of the most recently started application still running, if any.
    ///
    /// Used to label the guide menu when the compositor does not supply a
    /// foreground title — which is the case on any compositor other than
    /// Linboard, where the shell is an ordinary layer-shell client.
    pub fn running_app(&self) -> Option<&str> {
        self.launched_apps.last().map(|app| app.name.as_str())
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
                    let runtime_ms = now.duration_since(launched.started_at).as_millis();
                    if status.success() {
                        tracing::info!(
                            app = %launched.name,
                            command = %launched.command,
                            %status,
                            runtime_ms,
                            "application process exited"
                        );
                    } else {
                        tracing::warn!(
                            app = %launched.name,
                            command = %launched.command,
                            %status,
                            runtime_ms,
                            "application process exited unsuccessfully"
                        );
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
            category_position: 0.0,
            item_position: 0.0,
            category_speed: 0.0,
            item_speed: 0.0,
        }
    }

    pub fn selected_item(&self) -> usize {
        self.selected_items
            .get(self.selected_category)
            .copied()
            .unwrap_or(0)
    }

    pub fn current_category<'a>(&self, xmb: &'a Xmb) -> Option<&'a Category> {
        xmb.categories.get(self.selected_category)
    }

    pub fn current_app<'a>(&self, xmb: &'a Xmb) -> Option<&'a App> {
        self.current_category(xmb)?.apps.get(self.selected_item())
    }

    /// Apply a navigation action. Returns `true` if anything moved.
    ///
    /// Launching is not here: it acts on the shared catalogue rather than on
    /// one display's view of it, so the shell drives it directly.
    pub fn navigate(&mut self, action: Action, xmb: &Xmb) -> bool {
        if xmb.categories.is_empty() {
            return false;
        }

        match action {
            Action::Left => {
                if self.selected_category == 0 {
                    return false;
                }
                self.selected_category -= 1;
                self.restore_column();
                true
            }
            Action::Right => {
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
                self.selected_items[self.selected_category] = current - 1;
                true
            }
            Action::Down => {
                let count = xmb.categories[self.selected_category].apps.len();
                let current = self.selected_item();
                if current + 1 >= count {
                    return false;
                }
                self.selected_items[self.selected_category] = current + 1;
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
    fn restore_column(&mut self) {
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
        let target_item = self.selected_item() as f32;

        let step = |at: f32, speed: f32, target: f32| {
            let (at, speed) = linboard_protocol::overview::spring(
                at as f64,
                speed as f64,
                target as f64,
                EASE_RATE as f64,
                dt as f64,
            );
            (at as f32, speed as f32)
        };
        (self.category_position, self.category_speed) =
            step(self.category_position, self.category_speed, target_category);
        (self.item_position, self.item_speed) =
            step(self.item_position, self.item_speed, target_item);

        // Still moving while it is either away from its target or on its way
        // back to it: a spring an instant from crossing centre is at the
        // target and nowhere near finished.
        let moving = (target_category - self.category_position).abs() > SETTLED
            || (target_item - self.item_position).abs() > SETTLED
            || self.category_speed.abs() > SETTLED
            || self.item_speed.abs() > SETTLED;

        if !moving {
            // Snap so nothing renders at a fractional offset forever.
            self.category_position = target_category;
            self.item_position = target_item;
            self.category_speed = 0.0;
            self.item_speed = 0.0;
        }

        moving
    }
}

/// Start an application in an independent process session.
///
/// Linboard retains the returned handle only to collect its exit status. A
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
            // from the outer KDE session. Keep this window in Linboard's
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

fn executable_on_path(program: &OsStr) -> bool {
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
/// is accepted only through Linboard's explicit private-XWayland marker.
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
            .env("LINBOARD_XWAYLAND_DISPLAY", display);
    } else {
        command
            .env_remove("DISPLAY")
            .env_remove("LINBOARD_XWAYLAND_DISPLAY");
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
            path: PathBuf::from("/tmp/x.desktop"),
        }
    }

    fn model() -> Xmb {
        Xmb::with_wayland_display(
            vec![
                Category {
                    id: "a",
                    title: "A",
                    icon: "a",
                    apps: vec![app("a1"), app("a2"), app("a3")],
                },
                Category {
                    id: "b",
                    title: "B",
                    icon: "b",
                    apps: vec![app("b1")],
                },
            ],
            OsString::from("linboard-test"),
        )
    }

    fn cursor(xmb: &Xmb) -> Cursor {
        Cursor::new(xmb.categories.len())
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
        let xmb = Xmb::with_wayland_display(Vec::new(), OsString::from("linboard-test"));
        let mut cursor = cursor(&xmb);
        assert!(xmb.is_empty());
        assert!(!cursor.navigate(Action::Right, &xmb));
        assert!(cursor.current_app(&xmb).is_none());
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
        let mut child = launch("exit 23", false, OsStr::new("linboard-test"), None)
            .expect("the shell command should spawn");

        let status = child.wait().expect("the command should be waitable");
        assert_eq!(status.code(), Some(23));
    }

    #[test]
    fn model_reaps_finished_launches() {
        let mut xmb = model();
        xmb.categories[0].apps[0].exec = "exit 17".into();
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

    #[test]
    fn launched_apps_are_pinned_to_the_shell_wayland_socket() {
        let mut command = Command::new("true");
        command
            .env("WAYLAND_DISPLAY", "host-wayland")
            .env("WAYLAND_SOCKET", "23")
            .env("DISPLAY", ":1");

        confine_to_session(&mut command, OsStr::new("linboard-test"), None);

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(
            get("WAYLAND_DISPLAY"),
            Some(Some(OsStr::new("linboard-test")))
        );
        assert_eq!(get("WAYLAND_SOCKET"), Some(None));
        assert_eq!(get("DISPLAY"), Some(None));
        assert_eq!(get("LINBOARD_XWAYLAND_DISPLAY"), Some(None));
    }

    #[test]
    fn launched_x11_apps_receive_only_linboards_private_display() {
        let mut command = Command::new("true");
        command
            .env("DISPLAY", ":0")
            .env("LINBOARD_XWAYLAND_DISPLAY", ":0");

        confine_to_session(
            &mut command,
            OsStr::new("linboard-test"),
            Some(OsStr::new(":62")),
        );

        let get = |key: &str| {
            command
                .get_envs()
                .find(|(name, _)| *name == OsStr::new(key))
                .map(|(_, value)| value)
        };
        assert_eq!(get("DISPLAY"), Some(Some(OsStr::new(":62"))));
        assert_eq!(
            get("LINBOARD_XWAYLAND_DISPLAY"),
            Some(Some(OsStr::new(":62")))
        );
    }
}
