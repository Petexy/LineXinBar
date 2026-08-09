//! The context menu: the short list of things that can be done to whatever is
//! selected right now.
//!
//! Everything else the shell draws is a surface with its own layout — the bar,
//! the guide's sidebar, the keyboard. This is the one piece of furniture that
//! belongs to *no* screen in particular. It is raised over whichever one asked
//! for it, out of whatever control it is about, and it takes every button until
//! it is answered.
//!
//! So it is written as a component rather than as another screen. Whoever
//! raises it supplies three things and nothing else:
//!
//! * an **anchor** — the rectangle of the control the menu is about, in display
//!   coordinates, which is where the panel grows out of and folds back into;
//! * an optional **title**, which is what the anchor *is*, not what the menu
//!   does;
//! * the **entries**, built one line each.
//!
//! ```ignore
//! menu.open_at(
//!     tile_rect,
//!     Some(app.name.clone()),
//!     vec![
//!         Entry::new(Command::Placeholder("show-information"), "Show Information"),
//!         Entry::new(Command::Placeholder("hide"), "Hide from the Bar").disabled(),
//!         Entry::new(Command::Placeholder("remove"), "Remove").group(1).grave(),
//!     ],
//!     ui::context_menu_rows_that_fit(height),
//! );
//! ```
//!
//! Adding an entry later is one more line in that list plus one variant of
//! [`Command`]; raising the menu somewhere new is one more call. Neither costs
//! anything here, in the drawing, or in the input handling, and that is the
//! whole design.
//!
//! Nothing in this module knows how tall a display is or how the panel is
//! drawn. The one thing it has to be told is that last argument — how many rows
//! fit on the screen it was raised on — because a list longer than that
//! scrolls, and where the list is scrolled to is state, not layout. It is an
//! argument rather than something to remember to set, so that a menu raised
//! from somewhere new cannot be one that runs off the bottom of the display.

use std::time::Instant;

use crate::system::Level;

/// What choosing an entry asks the shell to do.
///
/// One variant per command the shell's context menus can carry. An enum rather
/// than a boxed callback because the menu is raised from one place and answered
/// in another, and a name that has to be matched somewhere is a name the
/// compiler can check: adding a command is a variant here and an arm wherever
/// menus are answered, and forgetting the second half does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Put up what the shell knows about the application the menu is about.
    Information,
    /// Ask whether to remove it from the machine.
    Uninstall,
    /// Answer that question with yes.
    ///
    /// Separate from [`Command::Uninstall`] rather than the same command
    /// arriving a second time: one of these opens a question and the other
    /// destroys something, and a shell where those are one name is a shell one
    /// mis-routed press away from removing an application nobody asked about.
    ConfirmUninstall,
    /// Hand what has been typed into the password field to the thing waiting
    /// for it. The one command that carries nothing with it: what was typed
    /// lives where the shell can look after it, not in a menu entry.
    SubmitPassword,
    /// Start it — or come back to it, if it is already running.
    Launch,
    /// Send the selected window to the display after this one, or the one
    /// before it.
    ///
    /// Two commands rather than one carrying a direction, because a menu entry
    /// is a thing the user chose by name: the row that says "next" and the row
    /// that says "previous" are as different as any other two rows, and a
    /// single command with a sign in it would be one place for the wrong sign
    /// to arrive from.
    MoveToNextDisplay,
    MoveToPreviousDisplay,
    /// Photograph the selected window and put the picture with the user's
    /// other ones.
    Screenshot,
    /// Silence the application this mixer row is about, or bring it back.
    ///
    /// The one command that names its subject. Every other row here is about
    /// whatever was selected when the menu was raised, which the shell can look
    /// up again when the row is chosen; a mixer lists several applications at
    /// once, so the row has to say which of them it is.
    MuteApplication(u32),
    /// The same for the session's own output — what the volume bar in the
    /// sidebar moves.
    ///
    /// Its own command rather than a number that stands for the session,
    /// because it is a different thing being silenced: one is an application,
    /// and this is the machine.
    MuteOutput,
    /// Open one of the user's own files — a song, a film, a photograph — in
    /// whatever their desktop already opens that kind of file with.
    ///
    /// Not [`Command::Launch`], although the shell carries both out the same
    /// way. What the row says is "Open", the thing it acts on is a file rather
    /// than an installation, and the two menus that carry them have nothing
    /// else in common: keeping one name for both would mean the day one of them
    /// grows a step the other must not take, there is nowhere to put it.
    Open,
    /// Ask *which* application, instead of taking the answer the desktop has
    /// already given.
    OpenWith,
    /// Open it with the `n`th of the applications that list offered.
    ///
    /// An index rather than a name, for the reason [`Command::MuteApplication`]
    /// carries a key: a command is copied around and matched on, the list it
    /// indexes is the one the shell built when it raised the menu, and a menu
    /// that is not up has no list to index.
    OpenWithHandler(usize),
    /// Put the selected file in the trash — after asking.
    Delete,
    /// Answer that question with yes. Separate from [`Command::Delete`] for the
    /// same reason [`Command::ConfirmUninstall`] is separate: one of these
    /// opens a question and the other takes somebody's photograph off the
    /// disk.
    ConfirmDelete,
    /// Ask what order the column should be listed in.
    Sort,
    /// List it in this one.
    SortBy(crate::media::Sort),
    /// Put the menu away and do nothing else. The row that says so out loud,
    /// for a user who has opened the menu and changed their mind; `B` does the
    /// same thing and is not discoverable.
    Dismiss,
    /// A row that is here to prove the menu works and has nothing behind it
    /// yet. The name is what gets logged when it is chosen, so a placeholder
    /// can be told from its neighbours while the real commands are written.
    ///
    /// Every one of these is meant to be replaced by a variant of its own, and
    /// every one of them since has been: no menu the shell raises today carries
    /// one. It stays because that is how the next menu starts — and because the
    /// tests for this module and for the drawing need a command that stands for
    /// any command, which is exactly what it is.
    #[allow(dead_code)]
    Placeholder(&'static str),
}

/// One row of the menu.
///
/// Built rather than matched on: the entries a menu offers depend on what it
/// was raised over — which application, which window, whether there is a second
/// display to move to — so they are assembled at the moment the menu opens
/// instead of being a fixed list somewhere.
///
/// Not `Eq`, and it cannot be: a row can carry a level, and a level is where a
/// control stands rather than which of a set of things it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub command: Command,
    pub label: String,
    /// Drawn at the head of the row, if the shell has such a glyph.
    pub glyph: Option<&'static str>,
    /// A picture of the thing the row is about, by icon name, drawn instead of
    /// a glyph and larger — an application's own icon rather than one of the
    /// shell's symbols. Anything the icon theme cannot answer for falls back to
    /// the generic application icon, which is the honest picture of a program
    /// the machine knows nothing else about.
    pub icon: Option<String>,
    /// Where the row's own control stands, for a row that is a track rather
    /// than a command.
    ///
    /// A track is *driven* rather than pressed: Left and Right move it, and the
    /// press the row still has changes the one thing about a level that is not
    /// a position — whether it is silenced. So a row with one of these does not
    /// put the panel away when it is chosen; see [`Menu::choose`].
    pub level: Option<Level>,
    /// Whether it can be chosen at all. A row that cannot is drawn as an
    /// outline rather than a chip and the highlight steps straight over it —
    /// the same answer in both places, for the same reason the guide's tiles
    /// use one: a control drawn as available that the selection then refuses to
    /// stop on is worse than either failure on its own.
    pub enabled: bool,
    /// Whether there is no coming back from choosing it. Drawn warmer, so the
    /// irreversible row is never picked by muscle memory alone.
    pub grave: bool,
    /// Whether it is the yes of a question that destroys something.
    ///
    /// Stronger than [`Self::grave`] and separate from it, because the two are
    /// answering different questions. A grave row is warm *within the palette*
    /// — it uses `Theme::danger`, which moves with the accent so that under the
    /// red theme it can go amber and stay distinct from being highlighted. That
    /// works for a row in a list of commands. It does not work for the Yes of a
    /// yes-or-no, where the colour is the only thing standing between the two
    /// buttons: those are drawn in [`crate::theme::DESTRUCTIVE`], which no
    /// accent touches.
    pub destructive: bool,
    /// Which band of the menu it belongs to. A rule is drawn wherever two
    /// neighbouring rows disagree, exactly as the guide's column does it, so
    /// grouping is a number on an entry rather than a separator entry that
    /// navigation would then have to skip.
    pub group: u8,
    /// Whether choosing it leaves the panel standing.
    ///
    /// The ordinary row is a way *off* the menu: it is chosen, it is watched
    /// going down, and the panel folds back into the control it grew out of.
    /// This one is a control *on* the menu — it answers by changing something
    /// the user is still looking at, and a panel that folded away would take
    /// the answer with it before it could be read. A mixer's tracks are like
    /// that, and so is a list of alternatives where exactly one is in force:
    /// the tick moving to the row just pressed *is* the answer, and the user
    /// leaves when they have finished setting it rather than because the shell
    /// decided one press was enough.
    pub holds: bool,
}

impl Entry {
    pub fn new(command: Command, label: impl Into<String>) -> Self {
        Self {
            command,
            label: label.into(),
            glyph: None,
            icon: None,
            level: None,
            enabled: true,
            grave: false,
            destructive: false,
            group: 0,
            holds: false,
        }
    }

    pub fn glyph(mut self, glyph: &'static str) -> Self {
        self.glyph = Some(glyph);
        self
    }

    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Make the row a track standing at `level`. A track is driven rather than
    /// pressed, so it holds the panel by definition.
    pub fn level(mut self, level: Level) -> Self {
        self.level = Some(level);
        self.holds()
    }

    /// Leave the panel standing when this row is chosen — see [`Entry::holds`].
    pub fn holds(mut self) -> Self {
        self.holds = true;
        self
    }

    /// Offer the row, but greyed out — for something that is a command here in
    /// general and simply cannot be done from where the user is standing.
    /// A row that is *never* possible should be left out instead.
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn grave(mut self) -> Self {
        self.grave = true;
        self
    }

    /// The yes of a question that destroys something. Also grave: everything
    /// true of an irreversible row is true of this one and more besides, which
    /// is why it is written as one built on the other rather than as two flags
    /// set side by side.
    pub fn destructive(self) -> Self {
        let mut destructive = self.grave();
        destructive.destructive = true;
        destructive
    }

    pub fn group(mut self, group: u8) -> Self {
        self.group = group;
        self
    }
}

/// How long the panel takes to grow out of its anchor, and to fall back into
/// it. The same in both directions: it is one journey, and a menu that left
/// faster than it arrived reads as having been dropped rather than put away.
pub const FLIGHT: f32 = 0.2;

/// How long a chosen row is watched going down before the panel starts folding
/// away. Short — a menu row is a button being pressed, not a switch being
/// thrown, so it wants the dip and not the latch.
pub const PRESS_TIME: f32 = 0.16;

/// How stiff the spring the highlight glides on is, in radians per second.
/// The guide's, so the two surfaces feel related.
const HIGHLIGHT_EASE_RATE: f32 = 21.0;

/// The menu as the shell holds it.
///
/// One per session rather than one per surface: only one can be up at a time
/// by definition — it takes every button while it is — so a second would be a
/// second answer to the question of who has the keys.
#[derive(Debug, Default)]
pub struct Menu {
    /// Whether it is taking input. Not the same question as whether it is on
    /// screen: a menu that has been answered hands the keys straight back while
    /// the panel is still folding into its anchor.
    open: bool,
    entries: Vec<Entry>,
    title: Option<String>,
    /// The control it was raised over, in display coordinates.
    anchor: [f32; 4],
    selected: usize,
    /// First row drawn, for a list longer than the display holds.
    scroll: usize,
    /// How many rows that is. Told to the menu by the shell, which is the only
    /// half of this that knows how big the screen is; zero means nobody has
    /// said yet, which is treated as "they all fit".
    window: usize,
    /// How far the panel is out of its anchor: 0 shut, 1 fully open.
    ///
    /// A position rather than the moment it opened, because it has to outlive
    /// the menu — see `open`. It is also what lets the two directions be one
    /// movement: a menu dismissed halfway out grows no further first.
    linear: f32,
    /// Eased rectangle of the selected row's chip, which slides down the
    /// column rather than jumping from row to row, and how fast it is going.
    highlight: Option<[f32; 4]>,
    highlight_speed: [f32; 4],
    /// The row being pressed and when the press started, and whether the panel
    /// is to fold away once it has finished. A press outlives the keystroke:
    /// the row has to be *seen* to go down, which takes longer than the frame
    /// the button went down on.
    pressed: Option<(usize, Instant)>,
    closing_after_press: bool,
    /// The list a row has asked for, waiting for that row to finish going down
    /// — see [`Menu::descend`].
    next: Option<(Option<String>, Vec<Entry>)>,
    /// The lists this one was reached through, innermost last, so [`Menu::back`]
    /// can put one of them back.
    stack: Vec<Step>,
}

/// A list the menu has stepped out of, held so it can be stepped back into.
///
/// The scroll is in here as well as the selection, because coming back to a
/// long list at the top of it and not where it was left is the same failure as
/// coming back to it with the wrong row highlighted.
#[derive(Debug)]
struct Step {
    title: Option<String>,
    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
}

impl Menu {
    // -- what is in it ------------------------------------------------------

    /// Raise the menu over `anchor` with these entries, on a display with room
    /// for `rows` of them. Returns whether it opened.
    ///
    /// A menu with nothing choosable in it does not open at all. A panel that
    /// grew out of a tile to offer four rows the highlight refuses to stop on
    /// is a dead end the user then has to press B to get out of, and the
    /// button that raised it is better read as having done nothing.
    pub fn open_at(
        &mut self,
        anchor: [f32; 4],
        title: Option<String>,
        entries: Vec<Entry>,
        rows: usize,
    ) -> bool {
        self.open_selecting(anchor, title, entries, rows, 0)
    }

    /// The same, opening on the first choosable row at or after `from` instead
    /// of at the top of the list.
    ///
    /// For a question whose safe answer is not its first one. A confirmation
    /// opens on No, so that a user who answers it the way they answer
    /// everything else — by pressing accept the moment something appears — has
    /// declined rather than agreed.
    pub fn open_selecting(
        &mut self,
        anchor: [f32; 4],
        title: Option<String>,
        entries: Vec<Entry>,
        rows: usize,
        from: usize,
    ) -> bool {
        if !entries.iter().any(|entry| entry.enabled) {
            return false;
        }
        self.window = rows;
        self.selected = entries
            .iter()
            .enumerate()
            .skip(from)
            .chain(entries.iter().enumerate())
            .find(|(_, entry)| entry.enabled)
            .map(|(index, _)| index)
            .unwrap_or_default();
        self.entries = entries;
        self.title = title;
        self.anchor = anchor;
        self.open = true;
        self.scroll = 0;
        self.pressed = None;
        self.closing_after_press = false;
        self.next = None;
        self.stack.clear();
        self.highlight = None;
        self.highlight_speed = [0.0; 4];
        self.keep_selection_in_view();
        true
    }

    /// Raise it with nothing in it at all.
    ///
    /// The one case a column of commands has no commands: a panel the shell is
    /// busy behind, where the honest set of things the user can usefully press
    /// is empty. It still takes the keys — that is what makes it modal — and it
    /// answers every one of them with nothing, which is what
    /// [`Self::move_selection`] and [`Self::choose`] already do for an empty
    /// list.
    ///
    /// Deliberately its own method rather than [`Self::open_at`] relaxing its
    /// guard. That guard exists because a menu whose rows can none of them be
    /// chosen is a dead end the user has to press B to escape, and it should go
    /// on refusing to open. This is not that: it is a panel that has not
    /// finished, and something else is on its way to replace it.
    pub fn open_waiting(&mut self, anchor: [f32; 4]) {
        self.window = 0;
        self.selected = 0;
        self.entries = Vec::new();
        self.title = None;
        self.anchor = anchor;
        self.open = true;
        self.scroll = 0;
        self.pressed = None;
        self.closing_after_press = false;
        self.next = None;
        self.stack.clear();
        self.highlight = None;
        self.highlight_speed = [0.0; 4];
    }

    // -- one list leading to another ---------------------------------------

    /// Step into a further list, out of the row that was just chosen.
    ///
    /// The panel stays exactly where it is and keeps the keys; what changes is
    /// what is written on it, and not until the row that asked for it has been
    /// seen to go down. That wait is the whole of why this is not simply
    /// [`Self::open_at`] a second time. A menu that swapped its rows on the
    /// frame of the press would take the pressed row's label away underneath
    /// the press, and one that folded into its anchor and grew back out would
    /// spend two thirds of a second saying nothing — for a step the user reads
    /// as going one level deeper into the same panel, which is what it is.
    ///
    /// Returns whether there was anything to step into. A list with nothing
    /// choosable in it is refused here for the same reason [`Self::open_at`]
    /// refuses one: it is a dead end with no way out but Back.
    pub fn descend(&mut self, title: Option<String>, entries: Vec<Entry>) -> bool {
        if !entries.iter().any(|entry| entry.enabled) {
            return false;
        }
        self.next = Some((title, entries));
        // Choosing a row handed the keys back. The step is not a way *off* the
        // panel, so it takes them again — and cancels the fold that was about
        // to start.
        self.open = true;
        self.closing_after_press = false;
        true
    }

    /// Whether a further list is on its way in, so a caller that is about to do
    /// something else with the panel knows one has been asked for.
    pub fn is_descending(&self) -> bool {
        self.next.is_some()
    }

    /// Step back out to the list this one was reached from. `false` when there
    /// is none, which is what tells the caller that Back means closing the
    /// whole panel.
    ///
    /// The highlight is deliberately not reset: it glides from wherever it is
    /// to the row being returned to, exactly as it would between two rows of
    /// one list. Coming back is a movement, not a new panel.
    pub fn back(&mut self) -> bool {
        // A list that has been asked for but has not arrived is simply
        // forgotten. The user has changed their mind inside the press.
        if self.next.take().is_some() {
            return true;
        }
        let Some(step) = self.stack.pop() else {
            return false;
        };
        self.title = step.title;
        self.entries = step.entries;
        self.selected = step.selected;
        self.scroll = step.scroll;
        self.pressed = None;
        self.closing_after_press = false;
        self.keep_selection_in_view();
        true
    }

    /// How deep in it is: zero on the list it was opened with.
    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Put the waiting list on the panel, keeping the one it replaces.
    fn enter(&mut self, title: Option<String>, entries: Vec<Entry>) {
        self.stack.push(Step {
            title: std::mem::replace(&mut self.title, title),
            entries: std::mem::replace(&mut self.entries, entries),
            selected: self.selected,
            scroll: self.scroll,
        });
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.enabled)
            .unwrap_or_default();
        self.scroll = 0;
        self.keep_selection_in_view();
    }

    /// Put it away. Returns whether it was open, so a caller can tell a
    /// dismissal from a press that has to go on to mean something else.
    ///
    /// The panel is not gone afterwards — it is falling back into its anchor,
    /// and [`Self::is_on_screen`] stays true until it has arrived.
    pub fn close(&mut self) -> bool {
        let was = self.open;
        self.open = false;
        self.closing_after_press = false;
        // Whatever it was in the middle of going into, it is not going there:
        // the panel folding into its anchor is the end of the whole journey,
        // not of the innermost list.
        self.next = None;
        self.stack.clear();
        was
    }

    /// Whether it is taking input.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether there is anything of it left to draw.
    pub fn is_on_screen(&self) -> bool {
        self.open || self.linear > 0.0 || self.pressed.is_some()
    }

    /// Whether something is still moving, so the display it is on keeps
    /// drawing frames until it has settled.
    pub fn is_animating(&self) -> bool {
        (self.linear > 0.0 && self.linear < 1.0) || self.pressed.is_some()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn anchor(&self) -> [f32; 4] {
        self.anchor
    }

    pub fn selected(&self) -> usize {
        self.selected.min(self.entries.len().saturating_sub(1))
    }

    // -- how much of it is on screen ---------------------------------------

    /// Say how many rows the display it is on has room for, after the fact —
    /// for a screen that changed size with the menu already up.
    ///
    /// Ordinarily this comes in with [`Self::open_at`]. It is held rather than
    /// passed in each time because the *scroll* is state: what the user can see
    /// has to survive between one press of Down and the next, and the only
    /// thing that decides it is how many rows fit.
    pub fn set_window(&mut self, rows: usize) {
        if rows == 0 || rows == self.window {
            return;
        }
        self.window = rows;
        self.keep_selection_in_view();
    }

    /// How many rows are drawn: every one of them, unless the screen is too
    /// short to hold them.
    pub fn visible_rows(&self) -> usize {
        match self.window {
            0 => self.entries.len(),
            window => window.min(self.entries.len()),
        }
    }

    /// The first row drawn.
    pub fn first_visible(&self) -> usize {
        self.scroll
            .min(self.entries.len().saturating_sub(self.visible_rows()))
    }

    /// Whether the list carries on above or below what is drawn, which is what
    /// the little arrows at the ends of the column are for.
    pub fn scrolled_above(&self) -> bool {
        self.first_visible() > 0
    }

    pub fn scrolled_below(&self) -> bool {
        self.first_visible() + self.visible_rows() < self.entries.len()
    }

    /// Follow the selection with the window, moving it as little as it takes.
    fn keep_selection_in_view(&mut self) {
        let rows = self.visible_rows();
        if rows == 0 {
            return;
        }
        let selected = self.selected();
        self.scroll = self
            .scroll
            .min(selected)
            .max((selected + 1).saturating_sub(rows))
            .min(self.entries.len().saturating_sub(rows));
    }

    // -- driving it ---------------------------------------------------------

    /// Move the highlight up or down. Returns whether it actually moved.
    ///
    /// Wraps, and skips whatever cannot be chosen. A context menu is short by
    /// nature — it is the things that can be done to one object — so running
    /// off the end is more annoying than surprising, and a disabled row is one
    /// the user has already been told, by its outline, is not for them.
    pub fn move_selection(&mut self, delta: i32) -> bool {
        let count = self.entries.len();
        if count == 0 {
            return false;
        }
        let current = self.selected();
        let mut next = current;
        for _ in 0..count {
            next = (next as i32 + delta).rem_euclid(count as i32) as usize;
            if self.entries[next].enabled {
                break;
            }
        }
        self.selected = next;
        self.keep_selection_in_view();
        next != current
    }

    /// Put the highlight straight on row `index`.
    ///
    /// What a pointer does: the panel stands still, so the row under the cursor
    /// simply is the selected row. A row that cannot be chosen is left alone
    /// rather than landed on — its outline has already said as much, and moving
    /// the highlight onto it would leave the user with a selection that answers
    /// nothing.
    ///
    /// The window is not moved. A row being pointed at is by definition a row
    /// already on screen, and scrolling to it would take it out from under the
    /// cursor that arrived on it.
    pub fn select(&mut self, index: usize) -> bool {
        if !self.entries.get(index).is_some_and(|entry| entry.enabled) {
            return false;
        }
        if self.selected() == index {
            return false;
        }
        self.selected = index;
        true
    }

    /// Choose the highlighted row: start its press, hand the keys back, and
    /// return what was asked for.
    ///
    /// The panel does not go anywhere yet. It stays exactly where it is until
    /// the row has been seen to go down, and only then folds into its anchor —
    /// so what the user watches is the button they pressed answering them,
    /// rather than a menu that vanished at the moment of the press.
    ///
    /// A row that holds is the exception, and keeps the keys: it is a control
    /// *on* the panel rather than a way off it, its answer is the panel itself
    /// changing, and one that folded away would take that answer with it
    /// before it could be seen. See [`Entry::holds`].
    pub fn choose(&mut self) -> Option<Command> {
        let index = self.selected();
        let entry = self.entries.get(index).filter(|entry| entry.enabled)?;
        let command = entry.command;
        let holds = entry.holds;
        self.pressed = Some((index, Instant::now()));
        self.closing_after_press = !holds;
        self.open = holds;
        Some(command)
    }

    /// The row the highlight is on, for a caller that has to know more about it
    /// than which command it carries.
    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected())
    }

    /// Put a fresh set of rows on an open panel.
    ///
    /// For a menu that is about something still moving — the mixer, whose rows
    /// are what the machine is playing right now. Two different things can have
    /// happened since the last look, and they are told apart by the commands
    /// alone:
    ///
    /// * the same rows standing somewhere else, which is a level being read
    ///   back or a slider the user has just moved. Nothing about the panel may
    ///   move: the highlight stays where it is, mid-glide if it is gliding, and
    ///   a long list stays scrolled where the user left it.
    /// * a different set of rows, because an application started or stopped
    ///   making a noise. Then the highlight follows the row it was on, if that
    ///   row is still there, and falls back to the first one that can be chosen.
    ///
    /// Reports whether anything at all changed, so the caller can leave the
    /// display alone when nothing did.
    pub fn refresh(&mut self, entries: Vec<Entry>) -> bool {
        if entries == self.entries {
            return false;
        }
        let same_rows = entries.len() == self.entries.len()
            && entries
                .iter()
                .zip(&self.entries)
                .all(|(fresh, held)| fresh.command == held.command);
        if same_rows {
            self.entries = entries;
            return true;
        }

        let was = self.selected_entry().map(|entry| entry.command);
        self.entries = entries;
        self.selected = was
            .and_then(|command| {
                self.entries
                    .iter()
                    .position(|entry| entry.command == command && entry.enabled)
            })
            .or_else(|| self.entries.iter().position(|entry| entry.enabled))
            .unwrap_or_default();
        // A row that has gone takes its press with it: what was being watched
        // going down is not on the panel any more.
        if self
            .pressed
            .is_some_and(|(index, _)| index != self.selected)
        {
            self.pressed = None;
        }
        self.keep_selection_in_view();
        true
    }

    /// How far through its press row `index` is, 0 at the row going down and 1
    /// once it is back. `None` when it is not being pressed, which is every row
    /// on almost every frame.
    pub fn press_progress(&self, index: usize) -> Option<f32> {
        let (pressed, at) = self.pressed?;
        if pressed != index {
            return None;
        }
        let progress = at.elapsed().as_secs_f32() / PRESS_TIME;
        (progress < 1.0).then_some(progress)
    }

    // -- the movement -------------------------------------------------------

    /// Advance the panel's growth by `dt` and return where it is now, 0 shut
    /// and 1 open.
    ///
    /// One number in both directions, so a menu dismissed before it finished
    /// opening falls back from where it is rather than snapping open first.
    pub fn animate(&mut self, dt: f32) -> f32 {
        // A press that has finished being watched is what releases the panel.
        // Until then the menu holds its ground: nothing here disappears before
        // its own transition has ended.
        if self
            .pressed
            .is_some_and(|(index, _)| self.press_progress(index).is_none())
        {
            self.pressed = None;
            self.closing_after_press = false;
        }
        // And a press that has finished is also what lets a further list on to
        // the panel — for the same reason and in the same breath. The row is
        // watched all the way down, and then the list it asked for arrives.
        if self.pressed.is_none() {
            if let Some((title, entries)) = self.next.take() {
                self.enter(title, entries);
            }
        }
        let held_open = self.open || self.closing_after_press;
        let target = if held_open { 1.0 } else { 0.0 };
        let step = dt / FLIGHT;
        self.linear = if self.linear < target {
            (self.linear + step).min(target)
        } else {
            (self.linear - step).max(target)
        };
        self.linear
    }

    /// One frame of the highlight's glide towards `target`, on the same
    /// critically damped spring the guide's chip rides: it leans into a move
    /// rather than leaving at full speed, and a second press part-way carries
    /// the first one's momentum on instead of starting again from rest.
    ///
    /// The first frame after opening snaps, so the glide is only ever between
    /// two real positions rather than in from nowhere.
    pub fn animate_highlight(&mut self, target: [f32; 4], dt: f32) -> [f32; 4] {
        let Some(current) = self.highlight else {
            self.highlight_speed = [0.0; 4];
            self.highlight = Some(target);
            return target;
        };
        let mut next = [0.0; 4];
        for ((slot, velocity), (from, to)) in next
            .iter_mut()
            .zip(self.highlight_speed.iter_mut())
            .zip(current.iter().zip(&target))
        {
            let (at, moving) = lxb_protocol::overview::spring(
                *from as f64,
                *velocity as f64,
                *to as f64,
                HIGHLIGHT_EASE_RATE as f64,
                dt as f64,
            );
            (*slot, *velocity) = (at as f32, moving as f32);
        }
        self.highlight = Some(next);
        next
    }

    /// Pretend the press started `seconds` ago, so tests can assert on the
    /// middle of the movement instead of racing it.
    #[cfg(test)]
    pub fn backdate_press(&mut self, seconds: f32) {
        if let Some((_, at)) = self.pressed.as_mut() {
            *at = at
                .checked_sub(std::time::Duration::from_secs_f32(seconds))
                .unwrap_or(*at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(labels: &[&'static str]) -> Vec<Entry> {
        labels
            .iter()
            .map(|label| Entry::new(Command::Placeholder(label), *label))
            .collect()
    }

    /// A menu whose display has room for every row it was given.
    fn open(labels: &[&'static str]) -> Menu {
        let count = labels.len();
        let mut menu = Menu::default();
        assert!(menu.open_at([10.0, 10.0, 40.0, 40.0], None, rows(labels), count));
        menu
    }

    #[test]
    fn a_menu_with_nothing_choosable_never_opens() {
        let mut menu = Menu::default();
        let dead: Vec<Entry> = rows(&["one", "two"])
            .into_iter()
            .map(Entry::disabled)
            .collect();
        assert!(!menu.open_at([0.0; 4], None, dead, 8));
        assert!(!menu.is_open());
        assert!(!menu.is_on_screen());
        assert!(!menu.open_at([0.0; 4], None, Vec::new(), 8));
    }

    #[test]
    fn it_opens_on_the_first_row_that_can_be_chosen() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two", "three"]);
        entries[0] = entries[0].clone().disabled();
        assert!(menu.open_at([0.0; 4], None, entries, 8));
        assert_eq!(menu.selected(), 1);
    }

    /// A pointer names the row it is on, and a row it may not choose refuses
    /// the highlight: a click on one must not press whatever was selected
    /// before, and the outline has already said it is not for pressing.
    #[test]
    fn a_row_can_be_pointed_at_and_a_disabled_one_refuses() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two", "three"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);

        assert!(menu.select(2));
        assert_eq!(menu.selected(), 2);
        // The row it is already on is not a move.
        assert!(!menu.select(2));

        assert!(!menu.select(1), "a disabled row is not selected by a click");
        assert_eq!(menu.selected(), 2);
        assert!(!menu.select(9));
    }

    #[test]
    fn the_highlight_steps_over_rows_it_cannot_stop_on() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two", "three"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);

        assert!(menu.move_selection(1));
        assert_eq!(menu.selected(), 2, "the disabled middle row was landed on");
        // And wraps, because the list is short by nature.
        assert!(menu.move_selection(1));
        assert_eq!(menu.selected(), 0);
        assert!(menu.move_selection(-1));
        assert_eq!(menu.selected(), 2);
    }

    #[test]
    fn a_single_choosable_row_reports_that_it_did_not_move() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);
        assert!(!menu.move_selection(1));
        assert_eq!(menu.selected(), 0);
    }

    /// The window follows the selection by as little as it takes, in both
    /// directions, and never runs off either end of the list.
    #[test]
    fn a_long_list_scrolls_under_a_fixed_window() {
        let mut menu = open(&["a", "b", "c", "d", "e", "f", "g"]);
        menu.set_window(3);
        assert_eq!(menu.visible_rows(), 3);
        assert_eq!(menu.first_visible(), 0);
        assert!(!menu.scrolled_above() && menu.scrolled_below());

        for _ in 0..2 {
            menu.move_selection(1);
        }
        assert_eq!((menu.selected(), menu.first_visible()), (2, 0));
        menu.move_selection(1);
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (3, 1),
            "the window moved further than the one row it had to"
        );
        assert!(menu.scrolled_above() && menu.scrolled_below());

        // Wrapping off the end takes the window with it, to the top.
        for _ in 0..4 {
            menu.move_selection(1);
        }
        assert_eq!((menu.selected(), menu.first_visible()), (0, 0));

        // And off the top to the bottom, where the window stops with the last
        // row against the foot of the panel rather than past it.
        menu.move_selection(-1);
        assert_eq!((menu.selected(), menu.first_visible()), (6, 4));
        assert!(menu.scrolled_above() && !menu.scrolled_below());
    }

    #[test]
    fn a_list_that_fits_never_scrolls() {
        let mut menu = open(&["a", "b"]);
        menu.set_window(6);
        assert_eq!(menu.visible_rows(), 2);
        assert!(!menu.scrolled_above() && !menu.scrolled_below());
        menu.move_selection(1);
        assert_eq!(menu.first_visible(), 0);
    }

    /// Choosing hands the keys back at once, but nothing disappears before its
    /// transition has finished: the panel is still on screen, and still fully
    /// open, for as long as the row is being watched going down.
    #[test]
    fn a_chosen_row_is_watched_before_the_panel_folds_away() {
        let mut menu = open(&["one", "two"]);
        while menu.animate(0.05) < 1.0 {}

        assert_eq!(menu.choose(), Some(Command::Placeholder("one")));
        assert!(!menu.is_open(), "it should hand the keys back at once");
        assert!(menu.is_on_screen());
        assert!(menu.press_progress(0).is_some());
        assert!(menu.press_progress(1).is_none());

        // Half way through the press, the panel has not begun to leave.
        menu.backdate_press(PRESS_TIME * 0.5);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0);

        // Once it has been seen, and only then, the panel folds into its
        // anchor — and stops being drawn when it gets there.
        menu.backdate_press(PRESS_TIME);
        assert!(menu.animate(1.0 / 60.0) < 1.0);
        assert!(menu.press_progress(0).is_none());
        while menu.animate(0.05) > 0.0 {}
        assert!(!menu.is_on_screen());
    }

    #[test]
    fn a_disabled_row_cannot_be_chosen() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);
        menu.move_selection(1);
        assert_eq!(menu.selected(), 0, "the highlight never reached it");
        assert!(menu.choose().is_some());
    }

    /// Dismissed half way out, it falls back from where it is rather than
    /// finishing its arrival first.
    #[test]
    fn dismissing_it_reverses_the_growth_from_where_it_is() {
        let mut menu = open(&["one"]);
        let half = menu.animate(FLIGHT * 0.5);
        assert!((0.0..1.0).contains(&half));
        assert!(menu.close());
        let after = menu.animate(1.0 / 60.0);
        assert!(after < half, "{after} should be below {half}");
        assert!(!menu.close(), "closing an closed menu is not a dismissal");
    }

    #[test]
    fn the_highlight_snaps_on_the_first_frame_and_glides_after_it() {
        let mut menu = open(&["one", "two"]);
        let first = [0.0, 0.0, 100.0, 40.0];
        assert_eq!(menu.animate_highlight(first, 1.0 / 60.0), first);

        let second = [0.0, 60.0, 100.0, 40.0];
        let stepped = menu.animate_highlight(second, 1.0 / 60.0);
        assert!(
            stepped[1] > first[1] && stepped[1] < second[1],
            "{stepped:?} should be on its way between the two rows"
        );
    }

    fn level(value: f32) -> Level {
        Level {
            value,
            muted: false,
        }
    }

    /// A track is a control on the panel, not a way off it: pressing one is
    /// still watched going down, but the panel it is on stays open — the answer
    /// to the press is the row itself, and a panel that folded away would take
    /// it with it.
    #[test]
    fn choosing_a_track_keeps_the_panel_open() {
        let mut menu = Menu::default();
        let entries = vec![
            Entry::new(Command::MuteApplication(7), "Something").level(level(0.5)),
            Entry::new(Command::Dismiss, "Close"),
        ];
        assert!(menu.open_at([0.0; 4], None, entries, 8));
        while menu.animate(0.05) < 1.0 {}

        assert_eq!(menu.choose(), Some(Command::MuteApplication(7)));
        assert!(menu.is_open(), "a mixer row does not hand the keys back");
        assert!(menu.press_progress(0).is_some(), "and is still watched");
        menu.backdate_press(PRESS_TIME);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0, "and nothing folded away");

        // The row below it is an ordinary command and behaves like one.
        menu.move_selection(1);
        assert_eq!(menu.choose(), Some(Command::Dismiss));
        assert!(!menu.is_open());
    }

    /// The levels moving is not the panel changing: the highlight stays where
    /// it is, mid-glide if it is gliding, and a scrolled list stays where the
    /// user left it.
    #[test]
    fn a_track_moving_leaves_the_panel_alone() {
        let rows = |value: f32| {
            vec![
                Entry::new(Command::MuteApplication(1), "One").level(level(value)),
                Entry::new(Command::MuteApplication(2), "Two").level(level(0.2)),
                Entry::new(Command::MuteOutput, "System").level(level(0.9)),
            ]
        };
        let mut menu = Menu::default();
        menu.open_at([0.0; 4], None, rows(0.5), 2);
        menu.move_selection(1);
        menu.move_selection(1);
        assert_eq!((menu.selected(), menu.first_visible()), (2, 1));

        assert!(menu.refresh(rows(0.6)), "the level moved");
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (2, 1),
            "nothing about the panel should have moved with it"
        );
        assert_eq!(menu.entries()[0].level, Some(level(0.6)));
        assert!(
            !menu.refresh(rows(0.6)),
            "and an unchanged listing is no news"
        );
    }

    /// An application that stops playing takes its row with it, and the
    /// highlight follows the row it was on rather than the position it was in.
    #[test]
    fn a_row_that_goes_away_takes_the_highlight_to_the_row_it_was_on() {
        let mut menu = Menu::default();
        menu.open_at(
            [0.0; 4],
            None,
            vec![
                Entry::new(Command::MuteApplication(1), "One").level(level(0.5)),
                Entry::new(Command::MuteApplication(2), "Two").level(level(0.5)),
                Entry::new(Command::MuteOutput, "System").level(level(0.5)),
            ],
            8,
        );
        menu.move_selection(1);
        menu.move_selection(1);
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::MuteOutput)
        );

        assert!(menu.refresh(vec![
            Entry::new(Command::MuteApplication(2), "Two").level(level(0.5)),
            Entry::new(Command::MuteOutput, "System").level(level(0.5)),
        ]));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::MuteOutput),
            "the highlight followed the row rather than staying on row two"
        );

        // And a highlight whose row has gone falls back to the first that can
        // be chosen rather than to wherever the index landed.
        assert!(menu.refresh(vec![
            Entry::new(Command::MuteApplication(2), "Two").level(level(0.5))
        ]));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::MuteApplication(2))
        );
    }

    /// A row that leads onward: the panel keeps the keys, keeps its place, and
    /// does not change what it says until the row has been seen going down.
    #[test]
    fn a_further_list_arrives_only_once_the_row_has_finished_going_down() {
        let mut menu = open(&["open", "sort"]);
        while menu.animate(0.05) < 1.0 {}
        menu.move_selection(1);

        assert_eq!(menu.choose(), Some(Command::Placeholder("sort")));
        assert!(!menu.is_open(), "choosing always hands the keys back first");
        assert!(menu.descend(Some("Sort".to_string()), rows(&["a-z", "z-a"])));
        assert!(menu.is_open(), "a step inward takes them straight back");
        assert!(menu.is_descending());

        // Half way down: still the list that was pressed.
        menu.backdate_press(PRESS_TIME * 0.5);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0, "the panel must not fold");
        assert_eq!(menu.entries().len(), 2);
        assert_eq!(menu.title(), None);
        assert_eq!(menu.depth(), 0);

        // And once it has been: the further list, from the top, on a panel
        // that never went anywhere.
        menu.backdate_press(PRESS_TIME);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0);
        assert_eq!(menu.title(), Some("Sort"));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::Placeholder("a-z"))
        );
        assert_eq!(menu.depth(), 1);
        assert!(!menu.is_descending());
    }

    /// Back is one level at a time, and the list is found as it was left —
    /// the same row highlighted, and a long one scrolled to the same place.
    #[test]
    fn back_returns_to_the_list_it_was_reached_from() {
        let mut menu = open(&["a", "b", "c", "d", "e"]);
        menu.set_window(2);
        for _ in 0..3 {
            menu.move_selection(1);
        }
        assert_eq!((menu.selected(), menu.first_visible()), (3, 2));

        menu.choose();
        menu.descend(None, rows(&["one", "two"]));
        menu.backdate_press(PRESS_TIME);
        menu.animate(1.0 / 60.0);
        assert_eq!(menu.entries().len(), 2);
        assert_eq!((menu.selected(), menu.first_visible()), (0, 0));

        assert!(menu.back());
        assert_eq!(menu.entries().len(), 5);
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (3, 2),
            "the list should be found exactly as it was left"
        );
        assert!(!menu.back(), "there is nothing behind the first list");
        assert!(
            menu.is_open(),
            "and Back off the end does not close it here"
        );
    }

    /// Changing your mind inside the press: the list that was asked for never
    /// arrives, and the panel is still the one that was pressed.
    #[test]
    fn a_step_inward_can_be_taken_back_before_it_lands() {
        let mut menu = open(&["open", "sort"]);
        menu.choose();
        menu.descend(None, rows(&["a-z"]));
        assert!(menu.back());
        menu.backdate_press(PRESS_TIME);
        menu.animate(1.0 / 60.0);
        assert_eq!(menu.entries().len(), 2);
        assert_eq!(menu.depth(), 0);
    }

    /// A further list with nothing choosable in it is refused, on the same
    /// ground an empty menu is: it would be a panel Back is the only way off.
    #[test]
    fn a_further_list_with_nothing_in_it_is_refused() {
        let mut menu = open(&["open", "sort"]);
        menu.choose();
        assert!(!menu.descend(None, Vec::new()));
        assert!(!menu.descend(
            None,
            rows(&["a"]).into_iter().map(Entry::disabled).collect()
        ));
        assert!(!menu.is_descending());
    }

    /// Putting the panel away abandons the whole journey, not the innermost
    /// list of it — a menu that folded away and came back one level deeper
    /// would be a menu that remembered something the user had left.
    #[test]
    fn closing_it_forgets_where_it_had_got_to() {
        let mut menu = open(&["open", "sort"]);
        menu.choose();
        menu.descend(None, rows(&["a-z", "z-a"]));
        menu.backdate_press(PRESS_TIME);
        menu.animate(1.0 / 60.0);
        assert_eq!(menu.depth(), 1);

        assert!(menu.close());
        assert_eq!(menu.depth(), 0);
        assert!(!menu.back());
    }

    #[test]
    fn a_reopened_menu_starts_from_the_top_again() {
        let mut menu = open(&["a", "b", "c"]);
        menu.set_window(2);
        menu.move_selection(1);
        menu.move_selection(1);
        assert_eq!((menu.selected(), menu.first_visible()), (2, 1));

        menu.open_at([0.0; 4], None, rows(&["a", "b", "c"]), 2);
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (0, 0),
            "the same button press should always leave it in the same place"
        );
    }
}
