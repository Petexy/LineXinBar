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
        let (at, moving) = linboard_protocol::overview::spring(
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
    /// Dismiss the overlay and go back to whatever was underneath.
    Resume,
    /// Kill the application whose card is selected beside the column.
    Close,
    /// Show the bar without closing the running application.
    Dashboard,
    /// The power button at the foot of the column: opens [`PowerItem`].
    Power,
}

/// Longest an application title may run inside a menu label. Sized for the
/// sidebar at its 280px floor: "Close " plus this many characters and the
/// ellipsis must still be one line there.
const LABEL_TITLE_CHARS: usize = 14;

fn ellipsize(title: &str) -> String {
    let mut chars = title.chars();
    let short: String = chars.by_ref().take(LABEL_TITLE_CHARS).collect();
    if chars.next().is_some() {
        format!("{}…", short.trim_end())
    } else {
        short
    }
}

/// Entries offered when a window is selected beside the column.
const WITH_WINDOW: &[Item] = &[Item::Resume, Item::Close, Item::Dashboard, Item::Power];
/// The card under the cursor is the start screen itself. There is nothing to
/// close, and nothing for Dashboard to do that the card does not already do —
/// with the bar selected, going to it *is* resuming it.
const WITHOUT_WINDOW: &[Item] = &[Item::Resume, Item::Power];

impl Item {
    /// Menu label. `target` names the window the Close entry would kill —
    /// the one whose card is selected, not necessarily the one in front.
    ///
    /// Titles are ellipsized: window titles routinely carry a document path
    /// or a terminal's whole working directory, and a label that wraps in
    /// the sidebar prints over the entry below it. The full title stays
    /// readable under the card itself.
    pub fn label(self, target: Option<&str>) -> String {
        match self {
            // Never "Resume Celeste": the card beside the column already says
            // what is being resumed, in far more detail than a label can.
            Item::Resume => "Resume".to_string(),
            Item::Close => match target.map(ellipsize) {
                Some(target) => format!("Close {target}"),
                None => "Close".to_string(),
            },
            Item::Dashboard => "Dashboard".to_string(),
            // Drawn as a glyph, so there is nothing to write.
            Item::Power => String::new(),
        }
    }
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
            PowerItem::Exit => "Exit Linboard Shell",
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

    /// The entries to offer. `closable` is whether the selected card is a real
    /// window — which is the only thing the column varies on, because both
    /// entries it adds are about that window.
    pub fn items(&self, closable: bool) -> &'static [Item] {
        if closable {
            WITH_WINDOW
        } else {
            WITHOUT_WINDOW
        }
    }

    /// Row of the highlighted entry.
    ///
    /// The Close entry comes and goes as the cards are scrolled, so the answer
    /// is looked up by entry rather than remembered as a number. An entry that
    /// has just disappeared falls back to Resume, which is the one row that is
    /// never destructive and never absent.
    pub fn selected_index(&self, closable: bool) -> usize {
        let items = self.items(closable);
        self.selected
            .and_then(|item| items.iter().position(|candidate| *candidate == item))
            .unwrap_or(0)
    }

    pub fn selected_item(&self, closable: bool) -> Option<Item> {
        self.items(closable)
            .get(self.selected_index(closable))
            .copied()
    }

    /// Move the highlight. Returns `true` when it actually moved.
    pub fn move_selection(&mut self, delta: i32, closable: bool) -> bool {
        let items = self.items(closable);
        if items.is_empty() {
            return false;
        }
        let current = self.selected_index(closable);
        // Wrapping: the column is short enough that running off the end is
        // more annoying than surprising.
        let next = (current as i32 + delta).rem_euclid(items.len() as i32) as usize;
        self.selected = Some(items[next]);
        next != current
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
    /// `focused` is whether this is the display being driven; `base` is the
    /// layer the bar sits on when it is not covering anything.
    pub fn surface_state(
        &self,
        focused: bool,
        app_running: bool,
        keep_grabbed: bool,
        launching: bool,
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

    #[test]
    fn selection_wraps_and_survives_the_application_exiting() {
        let mut guide = Guide::default();
        guide.open();

        // Down the four-entry column, then wrap back to the top.
        for expected in [Item::Close, Item::Dashboard, Item::Power, Item::Resume] {
            assert!(guide.move_selection(1, WINDOW));
            assert_eq!(guide.selected_item(WINDOW), Some(expected));
        }

        // Sitting on the power button when the application exits must not
        // index past the shorter column, nor land on something else.
        guide.move_selection(-1, WINDOW);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Power));
        assert_eq!(guide.selected_item(START_CARD), Some(Item::Power));
        assert_eq!(guide.selected_index(START_CARD), 1);
    }

    /// The reason the selection is held as an entry and not a row number:
    /// scrolling the cards onto the start screen drops two rows, and a
    /// remembered row 2 would mean "Dashboard" before and "Power" after.
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
        assert_eq!(guide.selected_index(WINDOW), 3);
        assert_eq!(guide.selected_index(START_CARD), 1);
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

    #[test]
    fn long_titles_are_ellipsized_so_labels_cannot_wrap() {
        let label = Item::Close.label(Some("project-linboard : bash — Konsole"));
        assert!(label.ends_with('…'), "{label:?} should be ellipsized");
        assert!(
            label.chars().count() <= "Close ".len() + LABEL_TITLE_CHARS + 1,
            "{label:?} is still too long"
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
                guide.surface_state(driven, true, false, true, Layer::Background),
                (Layer::Overlay, KeyboardInteractivity::None),
                "the splash is on the display it was started from, driven or not"
            );
        }
        // And the display goes straight back to where it was afterwards.
        assert_eq!(
            guide.surface_state(true, true, false, false, Layer::Background),
            (Layer::Background, KeyboardInteractivity::OnDemand)
        );
    }

    #[test]
    fn only_the_driven_display_rises_for_the_guide() {
        let mut guide = Guide::default();
        guide.open();

        assert_eq!(
            guide.surface_state(true, true, false, false, Layer::Background),
            (Layer::Overlay, KeyboardInteractivity::Exclusive)
        );
        for mode in [Mode::Menu, Mode::BarOverApp] {
            if mode == Mode::BarOverApp {
                guide.show_bar_over_app();
            }
            assert_eq!(
                guide.surface_state(false, true, false, false, Layer::Background),
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

    #[test]
    fn the_bar_yields_the_keyboard_to_a_running_application() {
        let guide = Guide::default();
        assert_eq!(
            guide.surface_state(true, false, false, false, Layer::Background),
            (Layer::Background, KeyboardInteractivity::Exclusive)
        );
        assert_eq!(
            guide.surface_state(true, true, false, false, Layer::Background),
            (Layer::Background, KeyboardInteractivity::OnDemand)
        );
        // Unless it was told not to.
        assert_eq!(
            guide.surface_state(true, true, true, false, Layer::Background),
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
