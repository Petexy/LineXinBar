//! The guide overlay: the way back out of a running application.
//!
//! Once an application is fullscreen it owns the screen and the keyboard, so
//! the shell needs a state it can be summoned into from outside — by the
//! controller's guide button, or by the compositor forwarding its guide
//! binding. That is what this models.
//!
//! Three presentations, and the surface configuration follows from which one
//! is active rather than being tracked separately:
//!
//! * [`Mode::Bar`] — the bar itself, behind anything that is running.
//! * [`Mode::Menu`] — a dimmed scrim and a short menu, over what is running.
//! * [`Mode::BarOverApp`] — the whole bar, over what is running, so another
//!   application can be picked without closing the current one.
//!
//! There is one guide for the session, not one per display: it belongs to
//! whichever display the user is driving, and follows them to the next.

use std::time::Instant;

use smithay_client_toolkit::shell::wlr_layer::{KeyboardInteractivity, Layer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bar,
    Menu,
    BarOverApp,
}

/// Which half of the open menu has the focus: the entry column on the left,
/// or the window cards beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Menu,
    Windows,
}

/// A directional move inside the open menu, whichever pane has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    Left,
    Right,
}

/// How stiff the spring a highlight glides on is, in radians per second.
/// Matches the bar's easing so the two feel related.
const HIGHLIGHT_EASE_RATE: f32 = 21.0;

/// How long a switch takes to go over, in seconds: down, and back up with a
/// little bounce. Long enough to be seen from a couch, short enough that a
/// second press lands before the first has finished being watched.
pub const PRESS_TIME: f32 = 0.34;

/// How long the power dialog takes to grow out of its button, and to fall back
/// into it. Short: it is a question, and the answer is already on screen — the
/// motion is there to say *where the dialog came from*, not to be watched.
const POWER_FLIGHT: f32 = 0.22;

/// One frame of a highlight's glide towards `target`, on the same critically
/// damped spring the cards ride: it leans into a move rather than leaving at
/// full speed, and a second press part-way carries the first one's momentum
/// on instead of starting the chip off again from rest.
///
/// `current` of `None` is the first frame after opening: it snaps, so the
/// glide is only ever between two real positions rather than in from nowhere.
fn ease_rect(
    current: Option<[f32; 4]>,
    speed: &mut [f32; 4],
    target: [f32; 4],
    dt: f32,
) -> [f32; 4] {
    let Some(current) = current else {
        *speed = [0.0; 4];
        return target;
    };
    let mut next = [0.0; 4];
    for ((slot, velocity), (from, to)) in next
        .iter_mut()
        .zip(speed.iter_mut())
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
    next
}

/// One entry in the guide menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// Whether the right stick moves the pointer inside the application in
    /// front. A square tile, not a row: see [`Item::is_tile`].
    Pointer,
    /// Opens the per-application volume mixer: a panel out of this tile with
    /// every application making a noise on it. A tile like the switch beside
    /// it, and the only one that opens something rather than turning something
    /// over — see [`Item::is_tile`].
    Mixer,
    /// How loud the session is. A bar, not a button.
    Volume,
    /// How bright the display the menu is on is.
    Brightness,
    /// Dismiss the overlay and go back to whatever was underneath.
    Resume,
    /// Kill the application whose card is selected beside the column.
    Close,
    /// Show the bar without closing the running application.
    Dashboard,
    /// The power button at the foot of the column: opens [`PowerItem`].
    Power,
}

/// The bands the column is divided into. A rule is drawn wherever two
/// neighbouring entries disagree about which one they are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Band {
    /// What the session sounds and looks like.
    Quick,
    /// What the menu does to the application in front of it.
    Window,
    /// What it does to the session.
    Session,
}

/// Which of the two bars an entry is, for the code that has to move one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    Volume,
    Brightness,
}

/// Which bars this machine turned out to have.
///
/// Neither is a given: a session with no mixer of any kind has no volume to
/// set, and a screen is only dimmable if the kernel or the monitor itself says
/// so. A row that cannot do anything is left out rather than drawn dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bars {
    pub volume: bool,
    pub brightness: bool,
}

impl Item {
    /// Whether this entry is one of the square tiles at the top of the column
    /// rather than a row spanning it.
    ///
    /// The two of them share one line, which is the only place the column is
    /// not one entry per row. They are tiles because of the switch: a switch is
    /// a thing with two states, and a full-width chip that changed only in tint
    /// would read as a row that had been selected rather than as a control that
    /// is on. The mixer is a tile because it stands beside one — a glyph in the
    /// same square, opening the panel that the sound belongs in.
    pub fn is_tile(self) -> bool {
        matches!(self, Item::Pointer | Item::Mixer)
    }

    /// The glyph drawn on a tile.
    pub fn glyph(self) -> Option<&'static str> {
        match self {
            Item::Pointer => Some(crate::icons::POINTER_STICK),
            Item::Mixer => Some(crate::icons::VOLUME_MIXER),
            _ => None,
        }
    }

    /// Menu label. `target` names the application the Close entry would end —
    /// the one behind the selected card, not necessarily the one in front.
    ///
    /// The *application's* name, never its window's title. A title is whatever
    /// document, page or tab the window happens to be showing, so a browser
    /// offers to close "(7) This $400 Handheld is replacing my…" when what the
    /// button ends is Firefox — gibberish on a button that kills something.
    /// The title still belongs under the card, which is where it does its job:
    /// telling two windows of the same application apart.
    ///
    /// Nothing is shortened here. The name is short by nature, and the shell
    /// fits a label to its chip when it draws it, measuring the run rather
    /// than counting its characters.
    pub fn label(self, target: Option<&str>) -> String {
        match self {
            // Never "Resume Celeste": the card beside the column already says
            // what is being resumed, in far more detail than a label can.
            Item::Resume => "Resume".to_string(),
            Item::Close => match target {
                Some(target) => format!("Close {target}"),
                None => "Close".to_string(),
            },
            Item::Dashboard => "Dashboard".to_string(),
            // Drawn as a glyph or as a track, so there is nothing to write.
            Item::Power | Item::Volume | Item::Brightness | Item::Pointer | Item::Mixer => {
                String::new()
            }
        }
    }

    /// Which bar this entry is, if it is one.
    ///
    /// A bar is slid rather than pressed: Left and Right move it, which is
    /// also why they are the two directions that do not cross to the window
    /// cards from a row like this.
    pub fn bar(self) -> Option<Bar> {
        match self {
            Item::Volume => Some(Bar::Volume),
            Item::Brightness => Some(Bar::Brightness),
            _ => None,
        }
    }

    fn band(self) -> Band {
        match self {
            Item::Pointer | Item::Mixer | Item::Volume | Item::Brightness => Band::Quick,
            Item::Resume | Item::Close => Band::Window,
            Item::Dashboard | Item::Power => Band::Session,
        }
    }
}

/// The column's lines, as spans over `items`: `(first, count)`.
///
/// Every entry is a line of its own except the tiles, which share theirs. The
/// menu moves by line rather than by entry — Up from Volume reaches the tile
/// row, not the second tile in it — so this is what both the navigation and
/// the layout are written against, and they cannot disagree about the shape of
/// the column because there is only one answer to ask.
pub fn lines(items: &[Item]) -> Vec<(usize, usize)> {
    let mut lines: Vec<(usize, usize)> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        match lines.last_mut() {
            // Tiles run on until something that is not one.
            Some((first, count)) if item.is_tile() && items[*first].is_tile() => *count += 1,
            _ => lines.push((index, 1)),
        }
    }
    lines
}

/// The rows a rule is drawn above: every place the column changes band.
///
/// The power button is skipped, in both directions. It is not in the column at
/// all — it sits in the sidebar's corner — so it neither gets a rule of its own
/// nor counts as the entry above the one after it.
pub fn separator_rows(items: &[Item]) -> Vec<usize> {
    let mut rows = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if *item == Item::Power {
            continue;
        }
        let above = items[..index]
            .iter()
            .rev()
            .find(|item| **item != Item::Power);
        if above.is_some_and(|above| above.band() != item.band()) {
            rows.push(index);
        }
    }
    rows
}

/// Which line of `lines` entry `index` sits on.
fn line_index(lines: &[(usize, usize)], index: usize) -> usize {
    lines
        .iter()
        .position(|(first, count)| index >= *first && index < first + count)
        .unwrap_or(0)
}

/// That line's span, or `None` for a column with nothing in it at all — which
/// [`Guide::items`] never produces, and which is answered rather than panicked
/// over because the callers are asking about a keypress.
fn line_at(lines: &[(usize, usize)], index: usize) -> Option<(usize, usize)> {
    lines.get(line_index(lines, index)).copied()
}

/// A choice in the power dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerItem {
    Suspend,
    Shutdown,
    Exit,
    Cancel,
}

/// The dialog's choices, in the order they are drawn.
const POWER_ITEMS: &[PowerItem] = &[
    PowerItem::Suspend,
    PowerItem::Shutdown,
    PowerItem::Exit,
    PowerItem::Cancel,
];

impl PowerItem {
    pub fn label(self) -> &'static str {
        match self {
            PowerItem::Suspend => "Suspend System",
            PowerItem::Shutdown => "Turn Off System",
            PowerItem::Exit => "Exit LineXinBar Shell",
            PowerItem::Cancel => "Cancel",
        }
    }

    /// Whether choosing this ends the session or the machine. Drawn warmer
    /// than the rest, so the two irreversible rows are never picked by
    /// muscle memory alone.
    pub fn is_grave(self) -> bool {
        matches!(self, PowerItem::Shutdown | PowerItem::Exit)
    }
}

#[derive(Debug, Default)]
pub struct Guide {
    mode: Option<Mode>,
    /// Held as the entry itself rather than a row number: the column grows and
    /// shrinks as the cards are scrolled, and an index would slide the
    /// highlight onto a different entry as it did.
    selected: Option<Item>,
    pane: Option<Pane>,
    selected_window: usize,
    /// Which power choice is highlighted, while the dialog is open.
    power: Option<usize>,
    /// How far the dialog is out of its button: 0 shut, 1 fully open.
    ///
    /// A position rather than a start time, because it has to outlive the
    /// dialog. Dismissing it takes the keyboard back at once while the panel
    /// is still falling into the button, so the drawing needs an answer for a
    /// dialog that, as far as everything else is concerned, has already gone.
    power_linear: f32,
    /// When the menu was last opened; drives the slide-in animation.
    opened_at: Option<Instant>,
    /// Eased position of the selected menu entry's chip, which slides down the
    /// column rather than jumping from row to row, and how fast it is going.
    menu_highlight: Option<[f32; 4]>,
    menu_highlight_speed: [f32; 4],
    /// Which quick-settings bars the machine has. Held here rather than passed
    /// in, because it is the one thing that changes the shape of the column
    /// without the user having done anything.
    bars: Bars,
    /// Whether the compositor can be asked to move the pointer, which is what
    /// the stick-pointer tile would be for. Off on any compositor but
    /// LineXinBar, and on one too old to have the request.
    pointer_control: bool,
    /// Whether there is an application in front for the pointer tile to be
    /// about. Without one the switch has nothing to be turned on for, and it
    /// is drawn as a control that cannot be reached rather than one that can
    /// be pressed to no effect.
    pointer_target: bool,
    /// Which tile in the line the highlight is on was last chosen, so stepping
    /// down off the row and back up returns to the same one.
    selected_column: usize,
    /// The entry being pressed, and when the press started. A press outlives
    /// the keystroke: the switch has to be *seen* to go over, which takes
    /// longer than the frame the button went down on.
    pressed: Option<(Item, Instant)>,
}

impl Guide {
    pub fn mode(&self) -> Mode {
        self.mode.unwrap_or(Mode::Bar)
    }

    pub fn pane(&self) -> Pane {
        self.pane.unwrap_or(Pane::Menu)
    }

    /// Seconds since the menu was opened, for the entrance animation.
    pub fn age(&self) -> f32 {
        self.opened_at
            .map(|at| at.elapsed().as_secs_f32())
            .unwrap_or(f32::MAX)
    }

    /// Replay the entrance animation without resetting what is selected —
    /// for when the open menu follows the user to another display, which
    /// should feel like the menu arriving there, not teleporting.
    pub fn replay_entrance(&mut self) {
        if self.mode() == Mode::Menu {
            self.opened_at = Some(Instant::now());
            self.menu_highlight = None;
        }
    }

    /// Pretend the menu opened `seconds` ago, so tests can assert on the
    /// settled look instead of racing the entrance animation.
    #[cfg(test)]
    pub fn backdate_open(&mut self, seconds: f32) {
        if let Some(at) = self.opened_at.as_mut() {
            *at = at
                .checked_sub(std::time::Duration::from_secs_f32(seconds))
                .unwrap_or(*at);
        }
    }

    /// Index of the highlighted window card, clamped to the current list —
    /// cards disappear when their window closes under an open menu.
    pub fn selected_window(&self, count: usize) -> usize {
        self.selected_window.min(count.saturating_sub(1))
    }

    /// Move within the open menu. The two panes are one keyboard space:
    /// Right leaves the entry column for the cards, Left from any card comes
    /// back, and inside the cards Up/Down step the vertical column — which
    /// stops at its ends rather than wrapping, so holding a direction always
    /// settles somewhere.
    pub fn move_focus(&mut self, direction: Move, window_count: usize) -> bool {
        match self.pane() {
            Pane::Menu => match direction {
                Move::Right if window_count > 0 => {
                    self.pane = Some(Pane::Windows);
                    self.selected_window = self.selected_window(window_count);
                    true
                }
                _ => false,
            },
            Pane::Windows => {
                let current = self.selected_window(window_count);
                match direction {
                    Move::Left => {
                        self.pane = Some(Pane::Menu);
                        true
                    }
                    Move::Up if current > 0 => {
                        self.selected_window = current - 1;
                        true
                    }
                    Move::Down if current + 1 < window_count => {
                        self.selected_window = current + 1;
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// The menu entry chip glides between rows, so pressing Down moves
    /// something rather than repainting it somewhere else.
    ///
    /// The card frame on the other side deliberately has no equivalent: the
    /// cards are already gliding to their slots, and a frame easing towards a
    /// moving card chases it across the screen instead of marking it.
    pub fn animate_menu_highlight(&mut self, target: [f32; 4], dt: f32) -> [f32; 4] {
        let eased = ease_rect(
            self.menu_highlight,
            &mut self.menu_highlight_speed,
            target,
            dt,
        );
        self.menu_highlight = Some(eased);
        eased
    }

    pub fn is_menu(&self) -> bool {
        self.mode() == Mode::Menu
    }

    /// Whether the shell is drawing over a running application, and so needs
    /// to be on the overlay layer holding the keyboard.
    pub fn is_over_app(&self) -> bool {
        matches!(self.mode(), Mode::Menu | Mode::BarOverApp)
    }

    /// Say which quick-settings bars the machine turned out to have.
    ///
    /// Nothing has to be redrawn on the strength of it. The answer settles a
    /// second or so after startup, while the bar and not the menu is on
    /// screen, and by the time the column is next drawn it is simply the right
    /// shape; the one case where it can change with the menu already open —
    /// moving to a display with no brightness control — is a screen that is
    /// already being redrawn every frame for the selection pulse.
    pub fn set_bars(&mut self, bars: Bars) {
        self.bars = bars;
    }

    /// Say whether the pointer can be driven at all here.
    ///
    /// The same principle as the bars: a control the session cannot carry out
    /// is left out of the column rather than drawn dead. Without LineXinBar's
    /// own protocol there is no way to move a pointer that is not the shell's
    /// own drawing, and a switch that turned on a pointer nothing could see
    /// would be worse than no switch.
    pub fn set_pointer_control(&mut self, available: bool) {
        self.pointer_control = available;
    }

    /// Say whether there is an application for the tiles to be about.
    ///
    /// Unlike the bars, this is not allowed to change the *shape* of the
    /// column — see [`Self::items`] — so it changes what can be reached in it
    /// instead.
    pub fn set_pointer_target(&mut self, running: bool) {
        self.pointer_target = running;
    }

    /// Whether an entry can be chosen at all.
    ///
    /// The pointer tile is a switch about the application in front, so with
    /// none there is nothing for it to be. A switch that could still be pressed
    /// would be a control that does nothing when used exactly as intended,
    /// which is worse than one the highlight visibly refuses to stop on.
    ///
    /// The mixer is never in that position, whatever is running: the panel it
    /// opens always has the session's own output in it, so there is always
    /// something on the other side of the press.
    ///
    /// Everything else in the column always does something.
    pub fn is_enabled(&self, item: Item) -> bool {
        match item {
            Item::Pointer => self.pointer_target,
            _ => true,
        }
    }

    /// Whether any entry on a line can be chosen. A line where none can is
    /// stepped straight over.
    fn line_is_reachable(&self, items: &[Item], (first, count): (usize, usize)) -> bool {
        items[first..first + count]
            .iter()
            .any(|item| self.is_enabled(*item))
    }

    // -- pressing one ------------------------------------------------------

    /// Start the press animation on `item`.
    pub fn press(&mut self, item: Item) {
        self.pressed = Some((item, Instant::now()));
    }

    /// How far through its press `item` is, 0 at the button going down and 1
    /// once the switch has finished going over. `None` when it is not being
    /// pressed at all, which is every entry on almost every frame.
    pub fn press_progress(&self, item: Item) -> Option<f32> {
        let (pressed, at) = self.pressed?;
        if pressed != item {
            return None;
        }
        let progress = at.elapsed().as_secs_f32() / PRESS_TIME;
        (progress < 1.0).then_some(progress)
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

    /// Whether a press is still playing, so the caller knows to keep drawing.
    pub fn pressing(&self) -> bool {
        self.pressed
            .is_some_and(|(item, _)| self.press_progress(item).is_some())
    }

    /// The entries to offer.
    ///
    /// Three things vary the column. The tiles and the bars at the top are
    /// whatever this session can actually change. `closable` is whether the
    /// selected card is a real window, which governs the two entries that are
    /// about that window — with the start screen selected there is nothing to
    /// close, and nothing for Dashboard to do that the card does not already
    /// do, because going to the bar *is* resuming it.
    pub fn items(&self, closable: bool) -> Vec<Item> {
        let mut items = Vec::with_capacity(8);
        // The tiles come first, and they do not come and go with the
        // application the way Close does. A switch that vanished when the
        // application it applies to exited would take the tile beside it
        // half a row across the sidebar every time something was closed, and
        // it is a switch for *next time* as much as for now: the point of
        // remembering it per application is that it outlives the process.
        if self.pointer_control {
            items.push(Item::Pointer);
        }
        items.push(Item::Mixer);
        if self.bars.volume {
            items.push(Item::Volume);
        }
        if self.bars.brightness {
            items.push(Item::Brightness);
        }
        items.push(Item::Resume);
        if closable {
            items.push(Item::Close);
            items.push(Item::Dashboard);
        }
        items.push(Item::Power);
        items
    }

    /// Row of the highlighted entry.
    ///
    /// The Close entry comes and goes as the cards are scrolled, and a bar
    /// comes and goes with the display the menu is on, so the answer is looked
    /// up by entry rather than remembered as a number. An entry that has just
    /// disappeared falls back to Resume, which is the one row that is never
    /// destructive and never absent — asked for by name, not as row zero,
    /// because the quick-settings bars sit above it when there are any.
    pub fn selected_index(&self, closable: bool) -> usize {
        let items = self.items(closable);
        let row = |wanted: Item| items.iter().position(|item| *item == wanted);
        self.selected
            // An entry that has stopped being reachable is treated exactly
            // like one that has gone: the application the pointer tile was
            // about can exit with the menu already open and the highlight
            // sitting on it.
            .filter(|item| self.is_enabled(*item))
            .and_then(row)
            .or_else(|| row(Item::Resume))
            .unwrap_or(0)
    }

    /// Whether the highlight is on a line with somewhere else to go in the
    /// given direction. What tells Left and Right apart from crossing to the
    /// cards, without moving anything to find out.
    pub fn can_move_in_line(&self, delta: i32, closable: bool) -> bool {
        let items = self.items(closable);
        self.along_line(&items, self.selected_index(closable), delta)
            .is_some()
    }

    pub fn selected_item(&self, closable: bool) -> Option<Item> {
        self.items(closable)
            .get(self.selected_index(closable))
            .copied()
    }

    /// Move the highlight up or down. Returns `true` when it actually moved.
    ///
    /// By *line*, not by entry: the two tiles share one, and Up from the
    /// volume bar should reach that line rather than walk through it. Which
    /// tile it lands on is the one it was left on, so stepping off the row and
    /// back does not quietly move the selection sideways.
    pub fn move_selection(&mut self, delta: i32, closable: bool) -> bool {
        let items = self.items(closable);
        if items.is_empty() {
            return false;
        }
        let lines = lines(&items);
        let current = self.selected_index(closable);
        let line = line_index(&lines, current);

        // Wrapping: the column is short enough that running off the end is
        // more annoying than surprising. Lines with nothing reachable on them
        // are stepped straight over rather than landed on and bounced off —
        // with nothing running that is the whole tile line, and a highlight
        // that visited it would stop on a switch it cannot throw.
        let mut next_line = line;
        for _ in 0..lines.len() {
            next_line = (next_line as i32 + delta).rem_euclid(lines.len() as i32) as usize;
            if self.line_is_reachable(&items, lines[next_line]) {
                break;
            }
        }

        let (first, count) = lines[next_line];
        // The remembered column where it can be had, and the nearest reachable
        // entry to it otherwise.
        let wanted = first + self.selected_column.min(count - 1);
        let next = (first..first + count)
            .filter(|index| self.is_enabled(items[*index]))
            .min_by_key(|index| index.abs_diff(wanted))
            .unwrap_or(wanted);
        self.selected = Some(items[next]);
        next != current
    }

    /// Put the highlight straight on `item`, wherever it is in the column.
    ///
    /// What a pointer does. The column stands still whatever is selected, so
    /// unlike the bar there is nothing here to chase: an entry under the cursor
    /// simply *is* the selected entry, the way a hovered key on the on-screen
    /// keyboard is the selected key. One highlight, one thing `A` acts on, and
    /// one place for the user to look.
    ///
    /// Refuses an entry that is not in the column, and one the highlight is not
    /// allowed to stop on — the pointer tile with nothing running is drawn as a
    /// control out of reach, and being pointed at is not a change of mind.
    pub fn select(&mut self, item: Item, closable: bool) -> bool {
        let items = self.items(closable);
        let Some(index) = items.iter().position(|candidate| *candidate == item) else {
            return false;
        };
        if !self.is_enabled(item) {
            return false;
        }
        if self.selected == Some(item) && self.pane() == Pane::Menu {
            return false;
        }
        // Where along its line it sits, so that stepping off the tiles with a
        // direction and back returns to the tile the pointer left the highlight
        // on rather than to the one the keys last used.
        if let Some((first, _)) = line_at(&lines(&items), index) {
            self.selected_column = index - first;
        }
        self.selected = Some(item);
        self.pane = Some(Pane::Menu);
        true
    }

    /// The same for the deck: put the highlight on card `index`.
    pub fn select_window(&mut self, index: usize, count: usize) -> bool {
        if index >= count {
            return false;
        }
        if self.pane() == Pane::Windows && self.selected_window(count) == index {
            return false;
        }
        self.pane = Some(Pane::Windows);
        self.selected_window = index;
        true
    }

    /// And for the power dialog, which only answers while it is open.
    pub fn select_power(&mut self, index: usize) -> bool {
        if self.power.is_none() || index >= POWER_ITEMS.len() || self.power_index() == index {
            return false;
        }
        self.power = Some(index);
        true
    }

    /// Move the highlight along the line it is on — which only the tiles have
    /// more than one entry in.
    ///
    /// Deliberately does not wrap, and says so by returning `false`: Right off
    /// the last tile is how the caller knows to cross to the window cards
    /// instead, the same as Right from any other row.
    pub fn move_in_line(&mut self, delta: i32, closable: bool) -> bool {
        let items = self.items(closable);
        let current = self.selected_index(closable);
        let Some(next) = self.along_line(&items, current, delta) else {
            return false;
        };
        let (first, _) = line_at(&lines(&items), current).unwrap_or((next, 1));
        self.selected_column = next - first;
        self.selected = Some(items[next]);
        true
    }

    /// The next reachable entry along the line from `current`, if there is one.
    ///
    /// Disabled entries are stepped over rather than stopped on, so the tile
    /// line behaves as though it held one tile whenever the switch beside the
    /// mixer has no application to be about.
    fn along_line(&self, items: &[Item], current: usize, delta: i32) -> Option<usize> {
        let (first, count) = line_at(&lines(items), current)?;
        let mut at = current as i32;
        loop {
            at += delta.signum();
            if at < first as i32 || at >= (first + count) as i32 {
                return None;
            }
            if self.is_enabled(items[at as usize]) {
                return Some(at as usize);
            }
        }
    }

    // -- the power dialog ---------------------------------------------------

    /// Whether the power dialog is in front of the menu. While it is, it takes
    /// every key: it is a modal question about ending the session.
    pub fn power_open(&self) -> bool {
        self.power.is_some()
    }

    pub fn power_items(&self) -> &'static [PowerItem] {
        POWER_ITEMS
    }

    /// Advance the dialog's growth by `dt` and return where it is now.
    ///
    /// One number in both directions, so a dialog dismissed before it finished
    /// opening falls back from where it is rather than snapping open first.
    pub fn animate_power(&mut self, dt: f32) -> f32 {
        let target = if self.power.is_some() { 1.0 } else { 0.0 };
        let step = dt / POWER_FLIGHT;
        self.power_linear = if self.power_linear < target {
            (self.power_linear + step).min(target)
        } else {
            (self.power_linear - step).max(target)
        };
        self.power_linear
    }

    pub fn power_index(&self) -> usize {
        self.power.unwrap_or(0).min(POWER_ITEMS.len() - 1)
    }

    pub fn power_item(&self) -> Option<PowerItem> {
        self.power.map(|_| POWER_ITEMS[self.power_index()])
    }

    /// Open it on the first choice — never on one that ends the session, so a
    /// double press of A cannot shut the machine down.
    pub fn open_power(&mut self) {
        self.power = Some(0);
    }

    pub fn close_power(&mut self) {
        self.power = None;
    }

    /// Move within the dialog. It does not wrap: Cancel is the last row, and
    /// wrapping from it back onto "Turn Off System" is the one place where a
    /// held direction should stop rather than carry on.
    pub fn move_power(&mut self, delta: i32) -> bool {
        let Some(current) = self.power else {
            return false;
        };
        let next = (current as i32 + delta).clamp(0, POWER_ITEMS.len() as i32 - 1) as usize;
        self.power = Some(next);
        next != current
    }

    /// Open the menu, always from the top so the same button press always does
    /// the same thing.
    pub fn open(&mut self) {
        self.mode = Some(Mode::Menu);
        self.selected = Some(Item::Resume);
        self.selected_column = 0;
        self.pressed = None;
        self.pane = Some(Pane::Menu);
        self.selected_window = 0;
        self.power = None;
        // Shut, not falling shut: a menu dismissed with the dialog up and
        // reopened straight away must not play the collapse it never showed.
        self.power_linear = 0.0;
        self.opened_at = Some(Instant::now());
        self.menu_highlight = None;
    }

    pub fn close(&mut self) {
        self.mode = Some(Mode::Bar);
        self.power = None;
    }

    pub fn show_bar_over_app(&mut self) {
        self.mode = Some(Mode::BarOverApp);
    }

    /// Where one display's surface must sit, and whether it takes the keyboard.
    ///
    /// Derived rather than set at each transition, so the bar and the overlay
    /// cannot disagree about who owns the keyboard — and, since it is decided
    /// per display, so the overlay cannot end up in front of every screen at
    /// once.
    ///
    /// `focused` is whether this is the display being driven; `keyboard` is
    /// whether the on-screen keyboard or its hint is on it; `base` is the
    /// layer the bar sits on when it is not covering anything.
    pub fn surface_state(
        &self,
        focused: bool,
        app_running: bool,
        keep_grabbed: bool,
        launching: bool,
        keyboard: bool,
        base: Layer,
    ) -> (Layer, KeyboardInteractivity) {
        // A launch splash is over the application it is waiting for — that is
        // the whole trick, the window maps underneath and is revealed rather
        // than appearing on top. It is on the display it was started from,
        // driven or not, and it takes no keys: the application should have
        // them the instant it is there, without the shell having to notice and
        // hand them back.
        if launching {
            return (Layer::Overlay, KeyboardInteractivity::None);
        }

        // A display nobody is driving stays put and keeps its hands off the
        // keyboard, whatever the guide is doing on the display that is.
        if !focused {
            return (base, KeyboardInteractivity::None);
        }

        // Drawn over an application: above it, and holding the keyboard
        // whatever the application would prefer.
        if self.is_over_app() {
            return (Layer::Overlay, KeyboardInteractivity::Exclusive);
        }

        // The on-screen keyboard is also drawn over the application — and
        // must not take the keyboard from it, which is not a nicety but the
        // condition of the thing working at all. Keyboard focus is what
        // carries text-input focus: the moment the shell takes the keys, the
        // application's text field deactivates, and the keyboard that came up
        // because a field was focused would put itself away again. So it is
        // driven from the controller, and the keys it types are sent through
        // the seat as any other keyboard's would be.
        if keyboard {
            return (Layer::Overlay, KeyboardInteractivity::None);
        }

        // Behind it, where a running application owns input. With nothing in
        // front, take focus outright: some compositors never pick an OnDemand
        // background layer for initial focus, which would leave the launcher
        // unusable at startup.
        let interactivity = if app_running && !keep_grabbed {
            KeyboardInteractivity::OnDemand
        } else {
            KeyboardInteractivity::Exclusive
        };
        (base, interactivity)
    }

    /// What the guide button does, from wherever the shell currently is.
    ///
    /// Returns whether the menu is now open, which is the shell's cue to
    /// start the start screen flying into its card.
    pub fn toggle(&mut self) -> bool {
        match self.mode() {
            Mode::Menu => {
                self.close();
                false
            }
            Mode::Bar | Mode::BarOverApp => {
                self.open();
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_button_toggles_the_menu_from_anywhere() {
        let mut guide = Guide::default();
        assert_eq!(guide.mode(), Mode::Bar);

        // Opening reports it, so the shell knows to fly the start screen in.
        assert!(guide.toggle());
        assert_eq!(guide.mode(), Mode::Menu);
        assert!(!guide.toggle());
        assert_eq!(guide.mode(), Mode::Bar);

        // From the bar shown over an application, it opens rather than closes.
        guide.show_bar_over_app();
        assert!(guide.toggle());
        assert_eq!(guide.mode(), Mode::Menu);
    }

    /// Whether the selected card is a window; the shorthand reads as the
    /// situation it means.
    const WINDOW: bool = true;
    const START_CARD: bool = false;

    /// A guide with nothing this machine can do: no bars, no pointer control.
    /// The mixer tile is there whatever the session is, because it is the
    /// shell's own doing rather than something it has to ask for.
    #[test]
    fn the_column_carries_the_mixer_tile_on_any_session() {
        let guide = Guide::default();
        assert_eq!(
            guide.items(START_CARD),
            vec![Item::Mixer, Item::Resume, Item::Power]
        );
    }

    #[test]
    fn the_window_entries_are_offered_only_when_a_window_is_selected() {
        let guide = Guide::default();
        assert!(guide.items(WINDOW).contains(&Item::Close));
        assert!(guide.items(WINDOW).contains(&Item::Dashboard));

        // The start screen's card is selected: nothing to kill, and nothing
        // for Dashboard to do that Resume does not already do from here.
        assert!(!guide.items(START_CARD).contains(&Item::Close));
        assert!(!guide.items(START_CARD).contains(&Item::Dashboard));

        // The power button is the one entry that is always there.
        for closable in [WINDOW, START_CARD] {
            assert_eq!(guide.items(closable).last(), Some(&Item::Power));
        }
    }

    /// The column stands still whatever is selected, so an entry under the
    /// pointer simply *is* the selected entry — one highlight, and one thing
    /// for the accept button to act on however the user reached it.
    #[test]
    fn an_entry_can_be_pointed_at() {
        let mut guide = Guide::default();
        guide.open();
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Resume));

        assert!(guide.select(Item::Close, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Close));
        // The entry it is already on is not a move.
        assert!(!guide.select(Item::Close, WINDOW));
        // Nor is one the column does not currently have: Close is offered only
        // while a window's card is the one selected.
        assert!(!guide.select(Item::Close, START_CARD));
    }

    /// A tile the highlight is not allowed to stop on does not take it from a
    /// click either. The pointer switch with nothing running is drawn as a
    /// control out of reach, and being pointed at is not a change of mind.
    #[test]
    fn a_tile_that_is_out_of_reach_refuses_the_pointer() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        guide.open();
        assert!(guide.items(WINDOW).contains(&Item::Pointer));

        assert!(!guide.select(Item::Pointer, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Resume));

        // With an application in front it is a switch like any other.
        guide.set_pointer_target(true);
        assert!(guide.select(Item::Pointer, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Pointer));
    }

    /// Pointing at a card crosses to the deck, and pointing back at an entry
    /// crosses back: the two panes are one space to a mouse as much as to the
    /// directions.
    #[test]
    fn pointing_crosses_between_the_column_and_the_deck() {
        let mut guide = Guide::default();
        guide.open();
        assert_eq!(guide.pane(), Pane::Menu);

        assert!(guide.select_window(2, 4));
        assert_eq!(guide.pane(), Pane::Windows);
        assert_eq!(guide.selected_window(4), 2);
        assert!(!guide.select_window(2, 4), "already there");
        assert!(!guide.select_window(9, 4), "past the end of the deck");

        assert!(guide.select(Item::Resume, WINDOW));
        assert_eq!(guide.pane(), Pane::Menu);
    }

    /// The power dialog answers the pointer only while it is up. A press that
    /// moved its highlight after it had been dismissed would leave the next
    /// one opening on something other than the first choice.
    #[test]
    fn the_power_dialog_takes_the_pointer_only_while_it_is_open() {
        let mut guide = Guide::default();
        guide.open();
        assert!(!guide.select_power(2));

        guide.open_power();
        assert_eq!(guide.power_index(), 0);
        assert!(guide.select_power(2));
        assert_eq!(guide.power_index(), 2);
        assert!(!guide.select_power(2));
        assert!(!guide.select_power(99));

        guide.close_power();
        assert!(!guide.select_power(1));
    }

    #[test]
    fn selection_wraps_and_survives_the_application_exiting() {
        let mut guide = Guide::default();
        guide.open();

        // Down the column to its foot.
        for expected in [Item::Close, Item::Dashboard, Item::Power] {
            assert!(guide.move_selection(1, WINDOW));
            assert_eq!(guide.selected_item(WINDOW), Some(expected));
        }

        // Sitting on the power button when the application exits must not
        // index past the shorter column, nor land on something else.
        assert_eq!(guide.selected_item(START_CARD), Some(Item::Power));
        assert_eq!(guide.selected_index(START_CARD), 2);

        // And on round to the top of the column rather than stopping there,
        // which is the tile line: the mixer is on it and is always a stop.
        assert!(guide.move_selection(1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));
    }

    /// The reason the selection is held as an entry and not a row number:
    /// scrolling the cards onto the start screen drops two rows, and a
    /// remembered row 3 would mean "Dashboard" before and "Power" after.
    #[test]
    fn the_highlight_stays_on_its_entry_when_the_column_shortens() {
        let mut guide = Guide::default();
        guide.open();
        for _ in 0..3 {
            guide.move_selection(1, WINDOW);
        }
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Power));

        // Power survives the column halving; the row it sits on does not.
        assert_eq!(guide.selected_item(START_CARD), Some(Item::Power));
        assert_eq!(guide.selected_index(WINDOW), 4);
        assert_eq!(guide.selected_index(START_CARD), 2);
    }

    /// Losing the selected entry must not silently select a destructive one.
    #[test]
    fn a_vanished_entry_falls_back_to_resume() {
        let mut guide = Guide::default();
        guide.open();
        guide.move_selection(1, WINDOW);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Close));
        assert_eq!(guide.selected_item(START_CARD), Some(Item::Resume));
    }

    #[test]
    fn the_power_dialog_opens_on_a_harmless_choice_and_does_not_wrap() {
        let mut guide = Guide::default();
        guide.open();
        assert!(!guide.power_open());

        guide.open_power();
        assert!(guide.power_open());
        assert_eq!(guide.power_item(), Some(PowerItem::Suspend));
        assert!(!guide.power_item().unwrap().is_grave());

        // Up at the top stays put rather than wrapping onto the last row.
        assert!(!guide.move_power(-1));
        assert_eq!(guide.power_item(), Some(PowerItem::Suspend));

        for expected in [PowerItem::Shutdown, PowerItem::Exit, PowerItem::Cancel] {
            assert!(guide.move_power(1));
            assert_eq!(guide.power_item(), Some(expected));
        }
        // And Down at Cancel does not carry on round to "Turn Off System".
        assert!(!guide.move_power(1));
        assert_eq!(guide.power_item(), Some(PowerItem::Cancel));
    }

    /// The dialog grows out of its button and falls back into it, which means
    /// it outlives the state that says it is open: dismissing it hands the
    /// keyboard back at once, while there is still a panel on screen.
    #[test]
    fn a_dismissed_dialog_keeps_a_position_after_it_stops_being_open() {
        let frame = 1.0 / 60.0;
        let settle = |guide: &mut Guide| {
            for _ in 0..60 {
                guide.animate_power(frame);
            }
        };

        let mut guide = Guide::default();
        guide.open();
        assert_eq!(guide.animate_power(frame), 0.0, "shut when the menu opens");

        guide.open_power();
        let first = guide.animate_power(frame);
        assert!(first > 0.0 && first < 1.0, "it grows rather than appearing");
        settle(&mut guide);
        assert_eq!(guide.animate_power(0.0), 1.0);

        guide.close_power();
        assert!(!guide.power_open(), "the keyboard goes back immediately");
        let falling = guide.animate_power(frame);
        assert!(
            falling > 0.0 && falling < 1.0,
            "the panel is still on screen"
        );
        settle(&mut guide);
        assert_eq!(guide.animate_power(0.0), 0.0);

        // Reversing mid-flight carries on from where it is instead of
        // finishing the movement it was making first.
        guide.open_power();
        guide.animate_power(frame * 3.0);
        let reversed_from = guide.animate_power(0.0);
        guide.close_power();
        assert!(guide.animate_power(frame) < reversed_from);

        // And a menu reopened over a dialog that was never dismissed on
        // screen starts shut, not falling shut.
        guide.open_power();
        settle(&mut guide);
        guide.open();
        assert_eq!(guide.animate_power(0.0), 0.0);
    }

    #[test]
    fn dismissing_the_menu_takes_the_power_dialog_with_it() {
        let mut guide = Guide::default();
        guide.open();
        guide.open_power();
        guide.close();
        assert!(!guide.power_open());

        // And it is never still open the next time the menu is summoned.
        guide.open();
        assert!(!guide.power_open());
    }

    #[test]
    fn focus_crosses_between_menu_and_cards_and_back() {
        let mut guide = Guide::default();
        guide.open();
        assert_eq!(guide.pane(), Pane::Menu);

        // Right with nothing running goes nowhere.
        assert!(!guide.move_focus(Move::Right, 0));
        assert_eq!(guide.pane(), Pane::Menu);

        // With windows it enters the cards on the first one.
        assert!(guide.move_focus(Move::Right, 3));
        assert_eq!(guide.pane(), Pane::Windows);
        assert_eq!(guide.selected_window(3), 0);

        // Down walks the column and stops at its end rather than wrapping;
        // Up walks back and stops at the top the same way.
        assert!(guide.move_focus(Move::Down, 3));
        assert_eq!(guide.selected_window(3), 1);
        assert!(guide.move_focus(Move::Down, 3));
        assert_eq!(guide.selected_window(3), 2);
        assert!(!guide.move_focus(Move::Down, 3));
        assert!(guide.move_focus(Move::Up, 3));
        assert_eq!(guide.selected_window(3), 1);

        // Left returns to the menu from any card, not just the first.
        assert!(guide.move_focus(Move::Left, 3));
        assert_eq!(guide.pane(), Pane::Menu);
        // And coming back resumes on the card that was selected.
        assert!(guide.move_focus(Move::Right, 3));
        assert_eq!(guide.selected_window(3), 1);
    }

    #[test]
    fn reopening_resets_focus_to_the_menu() {
        let mut guide = Guide::default();
        guide.open();
        guide.move_focus(Move::Right, 2);
        guide.move_focus(Move::Down, 2);
        guide.close();

        guide.open();
        assert_eq!(guide.pane(), Pane::Menu);
        assert_eq!(guide.selected_window(2), 0);
    }

    #[test]
    fn a_card_selection_survives_windows_closing() {
        let mut guide = Guide::default();
        guide.open();
        guide.move_focus(Move::Right, 4);
        guide.move_focus(Move::Down, 4);
        guide.move_focus(Move::Down, 4);
        assert_eq!(guide.selected_window(4), 2);

        // Two of the four windows close: the index clamps instead of
        // pointing past the end.
        assert_eq!(guide.selected_window(2), 1);
        assert_eq!(guide.selected_window(0), 0);
    }

    #[test]
    fn the_menu_chip_snaps_first_then_glides() {
        let mut guide = Guide::default();
        guide.open();

        // First frame: no previous position, so no flight from nowhere.
        let first = guide.animate_menu_highlight([100.0, 100.0, 400.0, 300.0], 1.0 / 60.0);
        assert_eq!(first, [100.0, 100.0, 400.0, 300.0]);

        // A new target is approached, not teleported to.
        let next = guide.animate_menu_highlight([500.0, 100.0, 200.0, 150.0], 1.0 / 60.0);
        assert!(next[0] > 100.0 && next[0] < 500.0);
        assert!(next[2] < 400.0 && next[2] > 200.0);
    }

    const BOTH_BARS: Bars = Bars {
        volume: true,
        brightness: true,
    };

    /// The bars are only offered where they can do something. Neither is a
    /// given: a session with no mixer at all has no volume, and most desktop
    /// monitors cannot be dimmed by anything but their own buttons.
    #[test]
    fn a_bar_is_offered_only_where_the_machine_has_the_control() {
        let mut guide = Guide::default();
        assert!(!guide.items(WINDOW).iter().any(|item| item.bar().is_some()));

        guide.set_bars(Bars {
            volume: true,
            brightness: false,
        });
        assert_eq!(guide.items(WINDOW).get(1), Some(&Item::Volume));
        assert!(!guide.items(WINDOW).contains(&Item::Brightness));

        guide.set_bars(BOTH_BARS);
        guide.set_pointer_control(true);
        assert_eq!(
            guide.items(WINDOW),
            vec![
                Item::Pointer,
                Item::Mixer,
                Item::Volume,
                Item::Brightness,
                Item::Resume,
                Item::Close,
                Item::Dashboard,
                Item::Power
            ]
        );
        // Losing the window still only takes the window's own entries.
        assert_eq!(
            guide.items(START_CARD),
            vec![
                Item::Pointer,
                Item::Mixer,
                Item::Volume,
                Item::Brightness,
                Item::Resume,
                Item::Power
            ]
        );
    }

    /// The stick pointer needs a compositor that can move one. Without that
    /// the switch is left out rather than drawn dead, as the bars are.
    #[test]
    fn the_pointer_tile_needs_a_compositor_that_can_move_a_pointer() {
        let mut guide = Guide::default();
        assert!(!guide.items(WINDOW).contains(&Item::Pointer));

        guide.set_pointer_control(true);
        assert_eq!(guide.items(WINDOW).first(), Some(&Item::Pointer));

        // And it goes again if the session it was bound to did.
        guide.set_pointer_control(false);
        assert!(!guide.items(WINDOW).contains(&Item::Pointer));
    }

    /// The rules mark where the column changes from one kind of thing to
    /// another. The power button is in neither reckoning: it is not in the
    /// column, it is in the sidebar's corner.
    #[test]
    fn a_rule_is_drawn_wherever_the_column_changes_its_mind() {
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.set_pointer_control(true);
        // Tiles, bars | Resume, Close | Dashboard
        assert_eq!(separator_rows(&guide.items(WINDOW)), vec![4, 6]);
        // The same, with the window's own two entries gone.
        assert_eq!(separator_rows(&guide.items(START_CARD)), vec![4]);

        // No rule between the tiles and the bars: they are the same band —
        // what the session sounds and looks and behaves like. With neither bar
        // the mixer tile is that whole band on its own, and the rule under it
        // is the one that was there before the tiles were.
        let plain = Guide::default();
        assert_eq!(separator_rows(&plain.items(WINDOW)), vec![1, 3]);
        assert_eq!(separator_rows(&plain.items(START_CARD)), vec![1]);
    }

    /// A bar is slid rather than pressed, and the two are told apart by the
    /// entry itself so that no caller has to keep a list. The tiles are told
    /// apart the same way, and are neither bars nor rows.
    #[test]
    fn every_entry_knows_which_kind_of_control_it_is() {
        assert_eq!(Item::Volume.bar(), Some(Bar::Volume));
        assert_eq!(Item::Brightness.bar(), Some(Bar::Brightness));
        for button in [Item::Resume, Item::Close, Item::Dashboard, Item::Power] {
            assert_eq!(button.bar(), None, "{button:?}");
            assert!(!button.is_tile(), "{button:?}");
            assert!(!button.label(Some("Celeste")).is_empty() || button == Item::Power);
        }
        // Neither bar carries a label: the track is the whole control.
        assert!(Item::Volume.label(None).is_empty());
        assert!(Item::Brightness.label(None).is_empty());

        // The tiles are switches: a glyph, no track, no label, and neither of
        // them is a bar that Left and Right would slide.
        for tile in [Item::Pointer, Item::Mixer] {
            assert!(tile.is_tile(), "{tile:?}");
            assert_eq!(tile.bar(), None, "{tile:?}");
            assert!(tile.label(Some("Celeste")).is_empty(), "{tile:?}");
            assert!(tile.glyph().is_some(), "{tile:?}");
        }
        assert!(Item::Volume.glyph().is_none());
    }

    /// The two tiles share one line, and everything else has one to itself.
    /// Both the navigation and the layout are written against this, which is
    /// why there is one answer rather than two.
    #[test]
    fn the_tiles_share_a_line_and_nothing_else_does() {
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.set_pointer_control(true);
        let items = guide.items(WINDOW);
        assert_eq!(
            lines(&items),
            vec![(0, 2), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1), (7, 1)]
        );

        // One tile on its own is still just a line with one entry on it.
        guide.set_pointer_control(false);
        let items = guide.items(START_CARD);
        assert_eq!(lines(&items), vec![(0, 1), (1, 1), (2, 1), (3, 1), (4, 1)]);
    }

    /// Up and Down move by *line*, so the tile row is one stop rather than
    /// two, and the tile they land on is the one that was left.
    #[test]
    fn the_tiles_are_one_stop_on_the_way_down_the_column() {
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.set_pointer_control(true);
        guide.set_pointer_target(true);
        guide.open();

        // Up from Resume: the bars, then the tile line — once, not twice, and
        // landing on the one tile that can be reached rather than walking
        // through the pair.
        for expected in [Item::Brightness, Item::Volume, Item::Pointer, Item::Power] {
            assert!(guide.move_selection(-1, WINDOW));
            assert_eq!(guide.selected_item(WINDOW), Some(expected));
        }

        // And back down through it the same way.
        for expected in [Item::Pointer, Item::Volume] {
            assert!(guide.move_selection(1, WINDOW));
            assert_eq!(guide.selected_item(WINDOW), Some(expected));
        }
    }

    /// With nothing running the tile line is still a stop — the mixer is on
    /// it and the mixer is always reachable — but the highlight lands on the
    /// mixer rather than on the switch that has nothing to be about.
    #[test]
    fn the_tile_line_lands_on_whichever_tile_can_be_reached() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        guide.open();

        // Up from Resume reaches the line, past the dead switch on its left.
        assert!(guide.move_selection(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));

        // An application arriving is all it takes for the other one to become
        // a place the highlight will stop.
        guide.set_pointer_target(true);
        assert!(guide.move_in_line(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Pointer));
    }

    /// And a line with nothing reachable on it is stepped over rather than
    /// landed on. No column the guide builds is in that state — the mixer
    /// keeps the tile line alive and every other line is a control that always
    /// works — so the rule is asserted where it lives.
    #[test]
    fn a_line_with_nothing_reachable_on_it_is_stepped_over() {
        let guide = Guide::default();
        let tiles = [Item::Pointer, Item::Mixer];
        assert!(guide.line_is_reachable(&tiles, (0, 2)), "the mixer is");
        assert!(
            !guide.line_is_reachable(&tiles, (0, 1)),
            "the switch is not"
        );
    }

    /// And an application *leaving* with the highlight already on the tile
    /// must not strand it on a switch that can no longer be thrown.
    #[test]
    fn losing_the_application_takes_the_highlight_off_the_tile() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        guide.set_pointer_target(true);
        guide.open();
        guide.move_selection(-1, WINDOW);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Pointer));

        guide.set_pointer_target(false);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Resume));
    }

    /// Right off the end of a line is not a wrap — it is how the caller knows
    /// to cross to the window cards, exactly as from any other row. An entry
    /// that cannot be reached is not an end to stop at: it is stepped over,
    /// and where there is nothing past it Right crosses as usual.
    #[test]
    fn moving_along_a_line_steps_over_what_cannot_be_reached() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);
        guide.set_pointer_target(true);
        guide.open();
        guide.move_selection(-1, WINDOW);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Pointer));

        // With an application in front both tiles can be thrown, so the line
        // is two stops and Right is the other one.
        assert!(guide.can_move_in_line(1, WINDOW));
        assert!(guide.move_in_line(1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));

        // Without one the switch is not a stop, so from the mixer Left leaves
        // the column rather than landing on a control that does nothing.
        guide.set_pointer_target(false);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));
        assert!(!guide.can_move_in_line(-1, WINDOW));
        assert!(!guide.move_in_line(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));
        assert!(!guide.can_move_in_line(1, WINDOW));

        // And a line with one entry on it never moves sideways at all.
        guide.move_selection(1, WINDOW);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Resume));
        assert!(!guide.can_move_in_line(1, WINDOW));
        assert!(!guide.can_move_in_line(-1, WINDOW));
    }

    /// Whether an entry can be chosen, in one place, because two answers —
    /// what the highlight will stop on and how the tile is drawn — are read
    /// from it.
    #[test]
    fn only_the_pointer_tile_is_ever_out_of_reach() {
        let mut guide = Guide::default();
        guide.set_pointer_control(true);

        // The switch is about an application, so with none it cannot be
        // thrown. The mixer is about the sound the machine is making, which
        // it is making whether or not anything is running.
        assert!(!guide.is_enabled(Item::Pointer));
        assert!(guide.is_enabled(Item::Mixer));
        guide.set_pointer_target(true);
        assert!(guide.is_enabled(Item::Pointer));
        assert!(guide.is_enabled(Item::Mixer));

        // Everything else in the column always does something.
        for item in [
            Item::Volume,
            Item::Brightness,
            Item::Resume,
            Item::Close,
            Item::Dashboard,
            Item::Power,
        ] {
            assert!(guide.is_enabled(item), "{item:?}");
        }
    }

    /// A switch has to be seen to go over, which takes longer than the frame
    /// the button went down on.
    #[test]
    fn a_press_plays_out_after_the_button_has_been_let_go_of() {
        let mut guide = Guide::default();
        guide.open();
        assert!(!guide.pressing());
        assert_eq!(guide.press_progress(Item::Pointer), None);

        guide.press(Item::Pointer);
        assert!(guide.pressing());
        let progress = guide.press_progress(Item::Pointer).expect("under way");
        assert!(progress < 0.2, "it starts at the beginning: {progress}");
        // One entry at a time: nothing else in the column is going down.
        assert_eq!(guide.press_progress(Item::Mixer), None);
        assert_eq!(guide.press_progress(Item::Resume), None);

        // And reopening the menu does not replay one that was never seen.
        guide.open();
        assert!(!guide.pressing());
    }

    /// The bars are near the top of the column, so opening the menu could
    /// easily land on one — and then A, the button that means "do the thing",
    /// would mute the session instead of resuming.
    #[test]
    fn opening_lands_on_resume_even_with_bars_above_it() {
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.open();
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Resume));

        // Up from Resume reaches them, and the wrap still comes out at the
        // right end of a column that is now several entries longer.
        assert!(guide.move_selection(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Brightness));
        assert!(guide.move_selection(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Volume));
        // Up on to the tile line, and on round to the foot of the column.
        assert!(guide.move_selection(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));
        assert!(guide.move_selection(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Power));
    }

    /// A control that goes away must not leave the highlight on a row that no
    /// longer exists — the same reason the selection is held as an entry
    /// rather than as a row number.
    #[test]
    fn losing_a_bar_underneath_the_highlight_falls_back_to_resume() {
        let mut guide = Guide::default();
        guide.set_bars(BOTH_BARS);
        guide.open();
        guide.move_selection(-1, WINDOW);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Brightness));

        // Moving to a monitor nothing can dim.
        guide.set_bars(Bars {
            volume: true,
            brightness: false,
        });
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Resume));
        assert_eq!(guide.selected_index(WINDOW), 2);
    }

    #[test]
    fn opening_always_starts_on_resume() {
        let mut guide = Guide::default();
        guide.open();
        guide.move_selection(1, WINDOW);
        guide.close();

        guide.open();
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Resume));
    }

    /// Resume never names the application: the card beside the column is
    /// showing it. Close does, because it kills the card under the cursor
    /// rather than whatever happens to be in front.
    #[test]
    fn only_close_names_what_it_acts_on() {
        assert_eq!(Item::Resume.label(Some("Celeste")), "Resume");
        assert_eq!(Item::Resume.label(None), "Resume");
        assert_eq!(Item::Close.label(Some("Celeste")), "Close Celeste");
        assert_eq!(Item::Close.label(None), "Close");
        assert_eq!(Item::Dashboard.label(None), "Dashboard");
        assert!(Item::Power.label(None).is_empty());
    }

    /// What the button says is what the button does, whole: the name arrives
    /// already reduced to an application, and cutting it short here would
    /// only cut it in a different place from where the sidebar's edge is.
    #[test]
    fn close_names_the_application_in_full() {
        assert_eq!(
            Item::Close.label(Some("KDE System Settings")),
            "Close KDE System Settings"
        );
    }

    /// The regression that made the menu appear on every screen at once: an
    /// open guide raised *all* the layer surfaces, not just the one being
    /// driven.
    /// A launch splash has to be *above* the window it is waiting for — that
    /// is the whole trick, the application maps underneath and is revealed
    /// rather than appearing on top of the shell. And it must not hold the
    /// keyboard, or the application would come up unable to be typed at.
    #[test]
    fn a_launch_splash_waits_above_the_window_it_is_waiting_for() {
        let guide = Guide::default();
        for driven in [true, false] {
            assert_eq!(
                guide.surface_state(driven, true, false, true, false, Layer::Background),
                (Layer::Overlay, KeyboardInteractivity::None),
                "the splash is on the display it was started from, driven or not"
            );
        }
        // And the display goes straight back to where it was afterwards.
        assert_eq!(
            guide.surface_state(true, true, false, false, false, Layer::Background),
            (Layer::Background, KeyboardInteractivity::OnDemand)
        );
    }

    #[test]
    fn only_the_driven_display_rises_for_the_guide() {
        let mut guide = Guide::default();
        guide.open();

        assert_eq!(
            guide.surface_state(true, true, false, false, false, Layer::Background),
            (Layer::Overlay, KeyboardInteractivity::Exclusive)
        );
        for mode in [Mode::Menu, Mode::BarOverApp] {
            if mode == Mode::BarOverApp {
                guide.show_bar_over_app();
            }
            assert_eq!(
                guide.surface_state(false, true, false, false, false, Layer::Background),
                (Layer::Background, KeyboardInteractivity::None),
                "{mode:?} must leave the displays nobody is driving alone"
            );
        }
    }

    #[test]
    fn only_one_display_ever_asks_for_the_keyboard() {
        let mut guide = Guide::default();
        // Whatever the guide is doing, and whether or not something is
        // running, an unfocused display never takes input — two displays
        // asking at once is what makes the compositor pick the wrong one.
        for mode in [Mode::Bar, Mode::Menu, Mode::BarOverApp] {
            match mode {
                Mode::Bar => guide.close(),
                Mode::Menu => guide.open(),
                Mode::BarOverApp => guide.show_bar_over_app(),
            }
            for app_running in [false, true] {
                for keep_grabbed in [false, true] {
                    let (_, interactivity) = guide.surface_state(
                        false,
                        app_running,
                        keep_grabbed,
                        false,
                        false,
                        Layer::Background,
                    );
                    assert_eq!(
                        interactivity,
                        KeyboardInteractivity::None,
                        "{mode:?} app_running={app_running} keep_grabbed={keep_grabbed}"
                    );
                }
            }
        }
    }

    /// The condition the whole on-screen keyboard rests on. Keyboard focus is
    /// what carries text-input focus, so a board that took the keys would take
    /// them from the field it exists to type into: the application's text
    /// input would deactivate, and the board would put itself away in the same
    /// breath it appeared.
    #[test]
    fn the_on_screen_keyboard_rises_above_the_application_without_taking_its_keys() {
        let guide = Guide::default();
        assert_eq!(
            guide.surface_state(true, true, false, false, true, Layer::Background),
            (Layer::Overlay, KeyboardInteractivity::None)
        );
        // Even when the shell was told to hold the keyboard regardless: the
        // debugging flag cannot be allowed to make the keyboard useless.
        assert_eq!(
            guide.surface_state(true, true, true, false, true, Layer::Background),
            (Layer::Overlay, KeyboardInteractivity::None)
        );
        // Not on displays nobody is driving.
        assert_eq!(
            guide.surface_state(false, true, false, false, true, Layer::Background),
            (Layer::Background, KeyboardInteractivity::None)
        );
        // And the menu wins if both somehow claim the display, because the
        // menu is the one that needs the keys.
        let mut guide = Guide::default();
        guide.open();
        assert_eq!(
            guide.surface_state(true, true, false, false, true, Layer::Background),
            (Layer::Overlay, KeyboardInteractivity::Exclusive)
        );
    }

    #[test]
    fn the_bar_yields_the_keyboard_to_a_running_application() {
        let guide = Guide::default();
        assert_eq!(
            guide.surface_state(true, false, false, false, false, Layer::Background),
            (Layer::Background, KeyboardInteractivity::Exclusive)
        );
        assert_eq!(
            guide.surface_state(true, true, false, false, false, Layer::Background),
            (Layer::Background, KeyboardInteractivity::OnDemand)
        );
        // Unless it was told not to.
        assert_eq!(
            guide.surface_state(true, true, true, false, false, Layer::Background),
            (Layer::Background, KeyboardInteractivity::Exclusive)
        );
    }

    #[test]
    fn overlay_modes_are_the_ones_drawn_above_an_application() {
        let mut guide = Guide::default();
        assert!(!guide.is_over_app());
        guide.open();
        assert!(guide.is_over_app());
        guide.show_bar_over_app();
        assert!(guide.is_over_app());
        guide.close();
        assert!(!guide.is_over_app());
    }
}
