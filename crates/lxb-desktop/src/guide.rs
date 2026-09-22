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

/// How long the menu takes to step back when the videos floating over it are
/// handed its directions, and to come forward again when it gets them back, in
/// seconds.
///
/// Shorter still. It is not a transition between two screens — it is one screen
/// saying which of two things the thumb is on, and an answer to that has to be
/// there before the next press is. Long enough to read as a movement, which is
/// the whole of what a hard cut would not be.
const ELSEWHERE_FLIGHT: f32 = 0.18;

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
    /// every application making a noise on it, and the shell's own audio
    /// under them. A tile like the switches beside it — see
    /// [`Item::is_tile`].
    Mixer,
    /// Whether anything is allowed to interrupt. On, an announcement is filed
    /// without a bubble and without a chime — it is still delivered, and the
    /// tile beside this one is where it is read. A switch like the pointer
    /// tile, and unlike it a switch about the session rather than about the
    /// application in front, so it is never inert.
    ///
    /// Beside the bell rather than anywhere else because the two are one
    /// question asked twice: this one says whether announcements may speak,
    /// and that one says what they said. Before it, because it is the setting
    /// and the list is its consequence.
    DoNotDisturb,
    /// Opens the notification list: everything that has been announced to the
    /// session, newest first, in the same panel the mixer is drawn in. A tile
    /// for the same reason the mixer is one — see [`Item::is_tile`] — and
    /// beside it because the two are the same kind of control: a glyph that
    /// raises a panel about one thing the session is doing.
    Notifications,
    /// How loud the session is. A bar, not a button.
    Volume,
    /// How loud the thing being played is, as against how loud the session is.
    ///
    /// Beside the session's own bar rather than beside the card it belongs to,
    /// because two grooves that both set a loudness are one thing the eye
    /// groups and reads at a glance — and because the question they answer
    /// together is the one somebody opens this menu with a film running to
    /// ask: *quieter, but which of the two.*
    ///
    /// Present only while something is playing, and it goes with the card.
    MediaVolume,
    /// How bright the display the menu is on is.
    Brightness,
    /// What is playing, and the three buttons that drive it.
    ///
    /// One entry rather than three. The card is a single control in the column
    /// — the highlight lands on all of it — and the three transport buttons are
    /// a selection *inside* it, walked with Left and Right the way a bar's
    /// level is. Three entries on one line would have been the tiles' shape,
    /// and it would have left the title under them belonging to nothing.
    ///
    /// Present only while something is playing. See [`Guide::animate_media`]
    /// for what "only while" means at the two ends of it.
    Media,
    /// Dismiss the overlay and go back to whatever was underneath.
    Resume,
    /// Kill the application whose card is selected beside the column.
    Close,
    /// Show the start screen without closing the running application.
    StartScreen,
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
    Media,
    Brightness,
}

/// One of the three buttons on the media card.
///
/// In the order they are drawn, which is also the order Left and Right walk
/// them, so the enum is the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Previous,
    PlayPause,
    Next,
}

/// The three, in the order they are drawn.
pub const TRANSPORT: [Transport; 3] = [Transport::Previous, Transport::PlayPause, Transport::Next];

impl Default for Transport {
    /// Play, which is the button a hand goes to without looking and the one
    /// the other two are found from.
    fn default() -> Self {
        Transport::PlayPause
    }
}

impl Transport {
    /// Where along the row it sits, which is what the selection eases between.
    pub fn column(self) -> usize {
        match self {
            Transport::Previous => 0,
            Transport::PlayPause => 1,
            Transport::Next => 2,
        }
    }
}

/// What the second corner card is about: work the machine is doing to itself
/// that nobody is standing in front of.
///
/// Today that is exactly one thing — an update installing while the person who
/// pressed it went off to do something else — and it is written here the way
/// [`crate::steam::Coming`] is written for a download, because the two are one
/// piece of furniture: a line saying what is happening and a bar saying how
/// far. See [`Guide::set_working`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Working {
    /// What is being done, in the words the panel behind it uses — "Updating
    /// System", "Preparing your updates".
    pub said: String,
    /// How far, or nothing where nothing can be counted yet. Nothing and not
    /// nought, for the reason a download's is nothing: an empty groove is the
    /// honest picture of a tool that has not said anything, and nought per
    /// cent is a reading.
    pub share: Option<f32>,
}

/// What the card is about: the one player the buttons act on.
///
/// Everything here is what the shell was told at the last look, and the card
/// goes on drawing it while it fades out — see [`Guide::animate_media`] — so a
/// player that has gone is still legible for as long as its card is on screen.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NowPlaying {
    /// The bus name a press is sent to. Not shown.
    pub bus: String,
    /// What is playing. Empty where the player says nothing, which is drawn as
    /// the application's own name instead — a card with a blank line under
    /// three buttons reads as one that failed to load.
    pub title: String,
    /// What to call it when there is no title.
    pub app: String,
    /// Whether it is playing *now*, which is the whole of what the middle
    /// button's glyph says: playing shows the pause mark, because a glyph on a
    /// button is what pressing it will do.
    pub playing: bool,
    pub can_previous: bool,
    pub can_next: bool,
    /// Where the playing application's own volume stands, and the row of the
    /// mixer that moves it. `None` where the sound server has nothing of the
    /// application in it — a video paused long enough for its stream to have
    /// been taken down — and then the groove is left out rather than drawn
    /// dead, exactly as a machine with no backlight leaves out the brightness
    /// bar.
    pub level: Option<crate::system::Level>,
    pub stream: Option<u32>,
}

impl NowPlaying {
    /// The line under the buttons.
    pub fn line(&self) -> &str {
        if self.title.is_empty() {
            &self.app
        } else {
            &self.title
        }
    }
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
    /// They share one line, which is the only place the column is not one
    /// entry per row. They are tiles because of the switches: a switch is a
    /// thing with two states, and a full-width chip that changed only in tint
    /// would read as a row that had been selected rather than as a control that
    /// is on. The other two are tiles because they stand beside one — a glyph
    /// in the same square, opening the panel the sound or the announcements
    /// belong in.
    pub fn is_tile(self) -> bool {
        matches!(
            self,
            Item::Pointer | Item::Mixer | Item::DoNotDisturb | Item::Notifications
        )
    }

    /// The glyph drawn on a tile.
    pub fn glyph(self) -> Option<&'static str> {
        match self {
            Item::Pointer => Some(crate::icons::POINTER_STICK),
            Item::Mixer => Some(crate::icons::VOLUME_MIXER),
            Item::DoNotDisturb => Some(crate::icons::DO_NOT_DISTURB),
            Item::Notifications => Some(crate::icons::NOTIFICATIONS),
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
            Item::Resume => crate::i18n::text("shell-resume").to_string(),
            Item::Close => match target {
                Some(target) => crate::message!("close-target", "target" => target),
                None => crate::i18n::text("shell-close").to_string(),
            },
            Item::StartScreen => crate::i18n::text("shell-start-screen").to_string(),
            // Drawn as a glyph or as a track, so there is nothing to write.
            Item::Power
            | Item::Volume
            | Item::MediaVolume
            | Item::Brightness
            | Item::Media
            | Item::Pointer
            | Item::Mixer
            | Item::DoNotDisturb
            | Item::Notifications => String::new(),
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
            Item::MediaVolume => Some(Bar::Media),
            Item::Brightness => Some(Bar::Brightness),
            _ => None,
        }
    }

    /// Whether this entry comes and goes with what the session is playing.
    ///
    /// The two of them do it together and at the same speed: the bar and the
    /// card are one control that happens to need two lines, and a session
    /// where the groove arrived a beat before the buttons would read as two
    /// unrelated things turning up at once.
    pub fn is_media(self) -> bool {
        matches!(self, Item::Media | Item::MediaVolume)
    }

    fn band(self) -> Band {
        match self {
            Item::Pointer
            | Item::Mixer
            | Item::DoNotDisturb
            | Item::Notifications
            | Item::Volume
            | Item::MediaVolume
            | Item::Brightness
            | Item::Media => Band::Quick,
            Item::Resume | Item::Close => Band::Window,
            Item::StartScreen | Item::Power => Band::Session,
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
    Restart,
    /// End the session and go back to the login screen. What used to sit here
    /// was "Exit LineXinBar Shell", which named the mechanism rather than the
    /// thing being asked for.
    LogOut,
    Cancel,
}

/// The dialog's choices, in the order they are drawn.
///
/// The two that act on the machine sit together, turning off before restarting
/// because that is the one people reach for without reading; leaving the
/// session follows them, and Cancel is always last.
const POWER_ITEMS: &[PowerItem] = &[
    PowerItem::Suspend,
    PowerItem::Shutdown,
    PowerItem::Restart,
    PowerItem::LogOut,
    PowerItem::Cancel,
];

impl PowerItem {
    pub fn label(self) -> &'static str {
        match self {
            PowerItem::Suspend => crate::i18n::text("shell-suspend-system"),
            PowerItem::Shutdown => crate::i18n::text("shell-turn-off-system"),
            PowerItem::Restart => crate::i18n::text("shell-restart-system"),
            PowerItem::LogOut => crate::i18n::text("shell-log-out"),
            PowerItem::Cancel => crate::i18n::text("shell-cancel"),
        }
    }

    /// Whether choosing this leaves the user with a machine that is off. Drawn
    /// warmer than the rest, so the one row nothing on this machine can undo
    /// is never picked by muscle memory alone.
    ///
    /// Only that row. Restarting and logging out both end the session, but
    /// both of them come back by themselves — what the warmth is for is the
    /// choice after which the user has to get up and press something.
    pub fn is_grave(self) -> bool {
        matches!(self, PowerItem::Shutdown)
    }
}

/// How long the media rows take to open, and to close again, in seconds.
///
/// Longer than the sidebar's own slide, because this happens *while the sidebar
/// is already up*: a row that appeared under the user's eye at the speed the
/// whole panel arrives at would read as a jump. Short enough that a track
/// starting while the menu is open is not a wait.
const MEDIA_FLIGHT: f32 = 0.34;

/// How long the selection takes to travel from one transport button to the
/// next.
///
/// Quick — the three buttons are a thumb's width apart, and a selection that
/// took as long to cross that as it takes to cross the column would lag the
/// press behind it.
const TRANSPORT_FLIGHT: f32 = 0.12;

/// How long the download card takes to come in from the edge, and to leave, in
/// seconds.
///
/// The media card's own flight, because it is the same event: something that
/// nobody pressed appearing inside a menu that is already up. A different
/// number would be two things arriving at two speeds in one corner of one
/// screen, which reads as a fault rather than as two answers.
const ARRIVING_FLIGHT: f32 = MEDIA_FLIGHT;

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
    /// How far the menu has stepped back while the videos floating over it have
    /// its directions: 0 with the menu in charge, 1 with a video in charge.
    ///
    /// A position rather than a start time, for the reason [`Guide::power_linear`]
    /// is one: a user who presses the stick twice in quick succession is
    /// watching the menu come back from wherever it had got to, not from where a
    /// restarted curve would put it.
    ///
    /// Whose the directions are is not the menu's own business to know — the
    /// shell holds that, and hands it in. What is the menu's business is how
    /// far it has got, which is a fact about the picture and belongs with the
    /// rest of them. See [`Guide::animate_elsewhere`].
    elsewhere_linear: f32,
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
    /// What the media card is about, or `None` for a session playing nothing.
    ///
    /// Kept for the whole of the card's life *including* its way out: the rows
    /// close over a third of a second and everything in them has to go on being
    /// drawn until they have — see [[motion-and-animation-rules]], and
    /// [`Guide::animate_media`], which is where this is finally dropped.
    showing: Option<NowPlaying>,
    /// Whether the shell still wants the card, as against whether it is still
    /// on screen. The target the openness below is easing towards.
    media_wanted: bool,
    /// The download in the corner of the menu, or `None` for a session with
    /// nothing coming down.
    ///
    /// Kept for the whole of the card's life *including* its way out, exactly
    /// as [`Guide::showing`] is: the card slides off over a third of a second
    /// and has to go on saying what it said until it has gone. Dropped in
    /// [`Guide::animate_download`], which is the only place that may.
    arriving: Option<crate::steam::Coming>,
    /// Whether the shell still wants that card. The target below eases to it.
    download_wanted: bool,
    /// How far the download card is in: 0 gone, 1 fully arrived. A position and
    /// not a start time, for the reason every other one here is — a card taken
    /// away half way in leaves from where it got to.
    download_linear: f32,
    /// The work the machine is doing to itself in the corner of the menu, or
    /// `None` for a session that is not updating. Kept through the way out on
    /// the terms [`Guide::arriving`] is kept, and dropped only by
    /// [`Guide::animate_working`].
    working: Option<Working>,
    /// Whether the shell still wants that card.
    working_wanted: bool,
    /// How far it is in: 0 gone, 1 fully arrived.
    working_linear: f32,
    /// Which row of the corner it stands on — 0 in the corner itself, 1 above
    /// a download card — eased rather than set, so that a download starting or
    /// ending under it moves it rather than teleporting it. See
    /// [`Guide::working_row`].
    working_row: f32,
    /// How far the media rows are open: 0 gone, 1 fully there.
    ///
    /// A position rather than a start time, for the reason [`Guide::power_linear`]
    /// is one: a card taken away halfway through arriving has to close from
    /// where it got to rather than snap open first.
    media_linear: f32,
    /// Which of the three transport buttons the highlight is on inside the
    /// card. Play is the middle one and the one a hand reaches for, so it is
    /// where the selection starts and where it goes back to when the card is
    /// closed and opened again.
    transport: Transport,
    /// Eased position of that selection along the three, in columns. The
    /// selection glides between the buttons rather than jumping — the same
    /// rule the highlight travelling down the column obeys.
    transport_at: f32,
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
    /// The three tiles beside it are never in that position, whatever is
    /// running: the mixer always has the session's own output in it, the
    /// notification list answers *nothing arrived* as readily as it lists what
    /// did, and whether the session may be interrupted is a question about the
    /// session — so there is always something on the other side of the press.
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
    /// close, and nothing for Start screen to do that the card does not already
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
        // Before the bell, because it is the setting and the list is what the
        // setting produced. Always, and never dimmed: whether the session may
        // be interrupted is a question that has an answer with nothing running
        // and nothing announced, and it is the one switch here that is worth
        // throwing *before* anything has arrived.
        items.push(Item::DoNotDisturb);
        // Always, and never dimmed: an empty list is an answer to the question
        // the tile asks — *did I miss anything* — and one the user is entitled
        // to get. A tile that could only be pressed once something had arrived
        // would be a control that appears the moment it is too late to have
        // learned where it was.
        items.push(Item::Notifications);
        if self.bars.volume {
            items.push(Item::Volume);
        }
        if self.bars.brightness {
            items.push(Item::Brightness);
        }
        // Then the two that come and go with what is playing, in that order and
        // after every bar the machine has of its own.
        //
        // The groove sat between the session's volume and the brightness bar
        // until 2026-08-20, on the reading that two grooves setting a loudness
        // belong together. On a machine that actually has a backlight that put
        // a control which comes and goes in the middle of two that never do,
        // and the user asked for it below — which is also the arrangement where
        // the two media rows are one block: what is playing, and how loud it
        // is, arriving and leaving together at the bottom of the quick
        // controls rather than through the middle of them.
        if self.has_media_volume() {
            items.push(Item::MediaVolume);
        }
        if self.has_media() {
            items.push(Item::Media);
        }
        items.push(Item::Resume);
        if closable {
            items.push(Item::Close);
            items.push(Item::StartScreen);
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

    /// Step the deck's highlight one card along, for a walk through the
    /// running applications rather than a direction pressed inside the menu.
    ///
    /// Wraps, alone among the ways of moving in the deck: [`Self::move_focus`]
    /// stops at both ends so that a held direction settles somewhere, and a
    /// walk is not a held direction. Somebody pressing the key a fourth time
    /// on three applications is asking to come back round to the first, not to
    /// be told they have run out.
    ///
    /// It also crosses into the deck from the entry column, which no direction
    /// but Right does — the walk is about the cards and nothing else, wherever
    /// the highlight happened to be standing when it began.
    /// A deck of one card is not walked at all: the step would land back on
    /// the card the highlight is already on, and the crossing into the deck
    /// would be the only thing that had happened.
    pub fn walk(&mut self, back: bool, count: usize) -> bool {
        if count < 2 {
            return false;
        }
        let current = self.selected_window(count) as i64;
        let step = if back { -1 } else { 1 };
        let next = (current + step).rem_euclid(count as i64) as usize;
        self.select_window(next, count)
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

    /// Whether the media rows are in the column at all.
    ///
    /// True from the moment something starts playing until the card has
    /// finished closing, which is deliberately longer than the shell wants it:
    /// a row taken out of `items` the instant the music stopped would leave the
    /// column to snap shut under whatever the user was looking at.
    pub fn has_media(&self) -> bool {
        self.showing.is_some()
    }

    /// Whether the groove is there too, which needs a stream to move as well
    /// as something playing.
    pub fn has_media_volume(&self) -> bool {
        self.showing.as_ref().is_some_and(|now| now.level.is_some())
    }

    /// What the card is about, for the drawing.
    pub fn now_playing(&self) -> Option<&NowPlaying> {
        self.showing.as_ref()
    }

    /// How far the media rows are open, 0 to 1.
    ///
    /// Read by the layout, which is asked from a dozen places that have no
    /// clock — so it is the eased position rather than anything computed from
    /// one, exactly as the power dialog's is.
    pub fn media(&self) -> f32 {
        self.media_linear
    }

    /// Tell the menu what the session is playing, or that it is playing
    /// nothing.
    ///
    /// Setting it while it is already up only refreshes what is written on it:
    /// a track changing must not restart the way in, and the selection must
    /// not move off the button under the user's thumb.
    pub fn set_now_playing(&mut self, now: Option<NowPlaying>) {
        match now {
            Some(now) => {
                if self.showing.is_none() {
                    // Coming back after being away: start where a hand would.
                    self.transport = Transport::default();
                    self.transport_at = Transport::default().column() as f32;
                }
                if self.showing.as_ref() != Some(&now) {
                    self.showing = Some(now);
                }
                self.media_wanted = true;
            }
            None => self.media_wanted = false,
        }
    }

    /// Advance the media rows by `dt` and return how far open they are.
    ///
    /// One number in both directions, like the power dialog's, so a card that
    /// is taken away while it is still arriving falls back from where it is.
    /// What this owns that the dialog does not is the *end* of the way out:
    /// the card is only forgotten once it has finished closing, which is what
    /// keeps its title and its buttons on screen for the whole of the fade.
    pub fn animate_media(&mut self, dt: f32) -> f32 {
        let target = if self.media_wanted { 1.0 } else { 0.0 };
        let step = dt / MEDIA_FLIGHT;
        self.media_linear = if self.media_linear < target {
            (self.media_linear + step).min(target)
        } else {
            (self.media_linear - step).max(target)
        };
        if !self.media_wanted && self.media_linear <= 0.0 {
            self.showing = None;
        }
        // And the selection inside the card, which glides between the three
        // buttons rather than jumping from one to the next.
        let wanted = self.transport().column() as f32;
        let step = dt / TRANSPORT_FLIGHT;
        self.transport_at = if self.transport_at < wanted {
            (self.transport_at + step).min(wanted)
        } else {
            (self.transport_at - step).max(wanted)
        };
        self.media_linear
    }

    /// How far the download card has arrived, 0 to 1.
    pub fn download(&self) -> f32 {
        self.download_linear
    }

    /// What that card is about, for as long as it is on screen.
    ///
    /// Still answering through the whole of the way out, which is what lets the
    /// card keep its name and its bar while it slides off — see
    /// [[motion-and-animation-rules]]: nothing here vanishes before its
    /// transition has ended.
    pub fn downloading(&self) -> Option<&crate::steam::Coming> {
        self.arriving.as_ref()
    }

    /// Put a download in the corner, take it away, or bring what is written on
    /// it up to date.
    ///
    /// Replacing what is on a card that is already up must not restart its way
    /// in: a percentage arrives every couple of seconds while something is
    /// downloading, and a card that flew in again at each of them would be a
    /// card nobody could read.
    pub fn set_downloading(&mut self, coming: Option<crate::steam::Coming>) {
        match coming {
            Some(coming) => {
                if self.arriving.as_ref() != Some(&coming) {
                    self.arriving = Some(coming);
                }
                self.download_wanted = true;
            }
            None => self.download_wanted = false,
        }
    }

    /// Advance the download card by `dt` and say how far in it is.
    ///
    /// One number in both directions, like the media card's, so a download that
    /// finishes while its card is still arriving leaves from where it is rather
    /// than snapping open first. And what it is about is only forgotten at the
    /// very end of the way out, which is what keeps the name and the bar on the
    /// card for the whole of the fade.
    pub fn animate_download(&mut self, dt: f32) -> f32 {
        let target = if self.download_wanted { 1.0 } else { 0.0 };
        let step = dt / ARRIVING_FLIGHT;
        self.download_linear = if self.download_linear < target {
            (self.download_linear + step).min(target)
        } else {
            (self.download_linear - step).max(target)
        };
        if !self.download_wanted && self.download_linear <= 0.0 {
            self.arriving = None;
        }
        self.download_linear
    }

    /// Whether the download card is still moving.
    ///
    /// In the shell's `pressing` beside [`Guide::media_is_moving`] and for the
    /// same reason: the card arrives because a download started, not because
    /// anything was pressed, so nothing else in the session is asking for the
    /// frames it needs to arrive in.
    pub fn download_is_moving(&self) -> bool {
        self.download_linear != if self.download_wanted { 1.0 } else { 0.0 }
    }

    /// How far the card about the machine's own work has arrived, 0 to 1.
    pub fn working_at(&self) -> f32 {
        self.working_linear
    }

    /// What that card is about, for as long as it is on screen — through the
    /// way out as well, exactly as [`Guide::downloading`] is.
    pub fn working(&self) -> Option<&Working> {
        self.working.as_ref()
    }

    /// Which row of the corner it stands on, eased: 0 in the corner, 1 one
    /// card's height above it.
    ///
    /// A number rather than a slot, because it moves. The corner belongs to
    /// the download — it was there first and it is the one a person is
    /// watching — so this card stands on top of it while there is one, and
    /// comes down into the corner when the download has finished leaving.
    /// Eased so that it is seen to come down rather than found somewhere
    /// else on the next frame.
    pub fn working_row(&self) -> f32 {
        self.working_row
    }

    /// Put the machine's own work in the corner, take it away, or bring what
    /// is written on it up to date.
    ///
    /// The same rule [`Guide::set_downloading`] keeps: replacing the words on
    /// a card that is already up must never restart its way in, because the
    /// percentage under them changes every second.
    ///
    /// Says whether anything about the card changed, because the card moves on
    /// a clock of its own: a job that is a per cent further along is a frame
    /// nothing else in the session is going to ask for. The download card
    /// beside it is kept moving the same way, off the library changing.
    pub fn set_working(&mut self, working: Option<Working>) -> bool {
        match working {
            Some(working) => {
                let changed = self.working.as_ref() != Some(&working) || !self.working_wanted;
                if changed {
                    self.working = Some(working);
                }
                self.working_wanted = true;
                changed
            }
            None => std::mem::replace(&mut self.working_wanted, false),
        }
    }

    /// Advance that card by `dt` — its way in, and its way down into the
    /// corner when the download under it has gone.
    pub fn animate_working(&mut self, dt: f32) -> f32 {
        let target = if self.working_wanted { 1.0 } else { 0.0 };
        let step = dt / ARRIVING_FLIGHT;
        self.working_linear = if self.working_linear < target {
            (self.working_linear + step).min(target)
        } else {
            (self.working_linear - step).max(target)
        };
        // A card with nothing under it belongs in the corner; one with a
        // download under it stands on top. Asked of the card rather than of
        // the download's position, because a download leaving is still
        // standing in the corner for the whole of its way out.
        let row = if self.arriving.is_some() { 1.0 } else { 0.0 };
        self.working_row = if self.working_linear <= 0.0 {
            // Nothing is on screen to be seen moving, so the next arrival
            // starts where it belongs rather than sliding in from the row the
            // last one happened to leave from.
            row
        } else if self.working_row < row {
            (self.working_row + step).min(row)
        } else {
            (self.working_row - step).max(row)
        };
        if !self.working_wanted && self.working_linear <= 0.0 {
            self.working = None;
        }
        self.working_linear
    }

    /// Whether that card is still moving — in, out, or between the two rows.
    pub fn working_is_moving(&self) -> bool {
        self.working_linear != if self.working_wanted { 1.0 } else { 0.0 }
            || (self.working_linear > 0.0
                && self.working_row != if self.arriving.is_some() { 1.0 } else { 0.0 })
    }

    /// Whether the media rows are still moving.
    ///
    /// The frames have to keep coming until they have settled, exactly as they
    /// do for a switch going over: a card arrives because a track started, not
    /// because anything was pressed, so nothing else in the session is asking
    /// for the frames its way in needs. Without this it opens in whatever
    /// single frame the shell happens to draw next, which is the pop-in the
    /// animation exists to prevent.
    pub fn media_is_moving(&self) -> bool {
        let target = if self.media_wanted { 1.0 } else { 0.0 };
        self.media_linear != target || self.transport_at != self.transport().column() as f32
    }

    /// Whether one transport button is a thing that can be pressed at all.
    ///
    /// The player is asked — a video with nothing after it says so — and play
    /// is always live, because a player that can be neither played nor paused
    /// is not a player the card would be up for.
    pub fn transport_is_live(&self, what: Transport) -> bool {
        let Some(now) = self.showing.as_ref() else {
            return false;
        };
        match what {
            Transport::Previous => now.can_previous,
            Transport::PlayPause => true,
            Transport::Next => now.can_next,
        }
    }

    /// Which transport button the highlight is on.
    ///
    /// Never one that cannot be pressed. A button the player will not answer is
    /// not a place the selection may rest, exactly as an entry that has stopped
    /// being reachable is not — see [`Guide::selected_index`], which falls back
    /// to Resume for the same reason this falls back to play. It matters here
    /// beyond the press: the selection is the one thing that changes a
    /// transport button's colour, so a highlight that could sit on a dead
    /// button would light it.
    pub fn transport(&self) -> Transport {
        if self.transport_is_live(self.transport) {
            self.transport
        } else {
            Transport::PlayPause
        }
    }

    /// Where that selection has got to along the row, in columns.
    pub fn transport_at(&self) -> f32 {
        self.transport_at
    }

    /// Whether Left or Right has anywhere to go inside the card.
    ///
    /// Off the end of the row they go back to meaning what they mean
    /// everywhere else in the column, which is how Right crosses to the window
    /// cards from the middle of a media row — the same bargain the tile line
    /// strikes.
    pub fn can_move_transport(&self, delta: i32, closable: bool) -> bool {
        self.next_transport(delta, closable).is_some()
    }

    /// The next button along that can actually be pressed, stepping over any
    /// that cannot — the same walk [`Guide::move_in_line`] makes along the
    /// tiles. A row whose only neighbour is dead has no neighbour.
    fn next_transport(&self, delta: i32, closable: bool) -> Option<Transport> {
        if self.selected_item(closable) != Some(Item::Media) {
            return None;
        }
        let mut at = self.transport().column() as i32;
        loop {
            at += delta;
            let next = *TRANSPORT.get(usize::try_from(at).ok()?)?;
            if self.transport_is_live(next) {
                return Some(next);
            }
        }
    }

    /// Move it, and say whether it went.
    pub fn move_transport(&mut self, delta: i32, closable: bool) -> bool {
        let Some(next) = self.next_transport(delta, closable) else {
            return false;
        };
        self.transport = next;
        true
    }

    /// Put the selection on one transport button outright, which is what a
    /// pointer over it does.
    ///
    /// Refused for a button that cannot be pressed: a pointer may not put the
    /// highlight somewhere the directions would step over.
    pub fn select_transport(&mut self, what: Transport, closable: bool) -> bool {
        if !self.transport_is_live(what) {
            return false;
        }
        let moved = self.select(Item::Media, closable) || self.transport != what;
        self.transport = what;
        moved
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

    /// Advance the menu's step back by `dt` and return where it is now.
    ///
    /// `away` is whether the directions are on one of the videos floating over
    /// the menu rather than on the menu — the shell's answer, since the shell is
    /// what holds them; see `Shell::floating_focus`.
    ///
    /// One number in both directions, like the dialog's, so a menu that gets
    /// its directions back before it has finished stepping away comes forward
    /// from where it is instead of snapping the rest of the way out first.
    pub fn animate_elsewhere(&mut self, away: bool, dt: f32) -> f32 {
        let target = if away { 1.0 } else { 0.0 };
        let step = dt / ELSEWHERE_FLIGHT;
        self.elsewhere_linear = if self.elsewhere_linear < target {
            (self.elsewhere_linear + step).min(target)
        } else {
            (self.elsewhere_linear - step).max(target)
        };
        self.elsewhere_linear
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
    /// carrying on round from it onto the rows that end the session is the one
    /// place where a held direction should stop rather than continue.
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
        // And forward, not coming forward, for exactly the same reason: a menu
        // put away while a video had its directions opens with them back.
        self.elsewhere_linear = 0.0;
        self.opened_at = Some(Instant::now());
        self.menu_highlight = None;
    }

    pub fn close(&mut self) {
        self.mode = Some(Mode::Bar);
        self.power = None;
    }

    pub fn show_start_screen_over_app(&mut self) {
        self.mode = Some(Mode::BarOverApp);
    }

    /// Put the menu away, leaving the bar where what is on the display
    /// underneath needs it to be.
    ///
    /// The two are not interchangeable and picking the wrong one is visible
    /// straight away: [`Self::close`] alone drops the shell below an
    /// application still in front of it, and [`Self::show_start_screen_over_app`] alone
    /// leaves it holding the overlay and the keyboard over an empty display.
    /// So every dismissal that is *not* the user choosing a row asks this
    /// instead of choosing for itself.
    pub fn dismiss(&mut self, app_running: bool) {
        if app_running {
            self.show_start_screen_over_app();
        } else {
            self.close();
        }
        self.power = None;
    }

    /// Where one display's surface must sit, and whether it takes the keyboard.
    ///
    /// Derived rather than set at each transition, so the bar and the overlay
    /// cannot disagree about who owns the keyboard — and, since it is decided
    /// per display, so the overlay cannot end up in front of every screen at
    /// once.
    ///
    /// `focused` is whether this is the display being driven; `keyboard` is
    /// whether the on-screen keyboard or its hint is on it; `typing_here` is
    /// whether that keyboard is typing into a field of the shell's own rather
    /// than into whatever is in front; `toasting` is whether the corner has a
    /// bubble in it and `volume` whether the volume keys have raised their
    /// control, both of which are the shell drawing over an application
    /// without taking anything from it; `base` is the layer the bar sits on
    /// when it is not covering anything.
    #[allow(clippy::too_many_arguments)]
    pub fn surface_state(
        &self,
        focused: bool,
        app_running: bool,
        keep_grabbed: bool,
        launching: bool,
        keyboard: bool,
        board_here: bool,
        typing_here: bool,
        toasting: bool,
        volume: bool,
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
        //
        // With one exception, and it is the only thing this shell draws over an
        // application on a screen the user is not driving: the on-screen
        // keyboard, where it has been pinned to a display of its own. The
        // surface has to be lifted or the board is painted behind the
        // application it was raised for. It still takes no keys — those are the
        // seat's and are settled below, on the display being driven.
        if !focused {
            return match keyboard && board_here {
                true => (Layer::Overlay, KeyboardInteractivity::None),
                false => (base, KeyboardInteractivity::None),
            };
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
        //
        // None of which is true of a board typing into the shell's own field —
        // a password, or the search at the head of a column. Nothing out there
        // has a text field to deactivate, nothing is sent through the seat, and
        // the shell is the one thing that wants the keys. It wants them *while*
        // the board is up, too, and not only afterwards: the board holds the
        // physical keyboard through the input method's grab, and the first key
        // pressed on it puts the board away and hands the keys back. A surface
        // that had given up focus for the board spends that handover with
        // nowhere for a keystroke to land, and the letter typed in the gap is
        // simply lost — which on a field somebody is typing a name into is one
        // character missing out of the middle of the word.
        //
        // The *lift* belongs to the display the board is drawn on, and the keys
        // do not. A board pinned to the second screen is over that screen's
        // application; this one has nothing of the board's on it, and lifting
        // it would put the whole bar over whatever is in front here — while the
        // keys still have to be handed over, because there is one seat and the
        // letters go wherever it points. The field the board types into is the
        // one thing that keeps the lift on this display: what is being typed
        // into is a panel this surface draws.
        if keyboard {
            return (
                if board_here || typing_here {
                    Layer::Overlay
                } else {
                    base
                },
                if typing_here {
                    KeyboardInteractivity::Exclusive
                } else {
                    KeyboardInteractivity::None
                },
            );
        }

        // Behind it, where a running application owns input. With nothing in
        // front, take focus outright: some compositors never pick an OnDemand
        // background layer for initial focus, which would leave the launcher
        // unusable at startup.
        if app_running && !keep_grabbed {
            // Unless something has been announced, or a volume key has been
            // pressed. A bubble in the corner and the control those keys raise
            // are the two things this shell draws over an application without
            // being asked to open anything, so both have to reach the overlay
            // layer — a notification the game is covering is a notification
            // that did not happen, and a volume bar behind the game is a key
            // that appears to do nothing.
            //
            // Exactly the launch splash's state, and for the same reason: the
            // shell is putting something in front of an application somebody is
            // still using, so it declines the keyboard, and `passes_pointer_through`
            // reads that pair and hands the clicks back too. The alternative —
            // an overlay that kept `OnDemand` — would be a transparent sheet
            // over the whole display swallowing every press for four seconds.
            // It matters more for the volume than for the corner: the hand
            // that pressed the key is on the keyboard of a game that is still
            // being played, and a control that took the keys for a second
            // would be a second of somebody's game played by nobody.
            //
            // Only this branch is lifted. The two states above it are already
            // over the application; the two the condition excludes are the
            // shell holding the keyboard on purpose — a dialog, or a start
            // screen with nothing in front — and neither of these may take the
            // keys off those. Neither needs the lift anyway: with nothing
            // running there is nothing to be behind.
            let over = toasting || volume;
            let layer = if over { Layer::Overlay } else { base };
            let interactivity = if over {
                KeyboardInteractivity::None
            } else {
                KeyboardInteractivity::OnDemand
            };
            return (layer, interactivity);
        }
        (base, KeyboardInteractivity::Exclusive)
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
        guide.show_start_screen_over_app();
        assert!(guide.toggle());
        assert_eq!(guide.mode(), Mode::Menu);
    }

    /// Whether the selected card is a window; the shorthand reads as the
    /// situation it means.
    const WINDOW: bool = true;
    const START_CARD: bool = false;

    /// A guide with nothing this machine can do: no bars, no pointer control.
    /// The three tiles left are there whatever the session is, because all
    /// three are the shell's own doing rather than something it has to ask
    /// for — and the switch among them is about the session rather than about
    /// an application, so it does not come and go with one either.
    #[test]
    fn the_column_carries_the_panel_tiles_on_any_session() {
        let guide = Guide::default();
        assert_eq!(
            guide.items(START_CARD),
            vec![
                Item::Mixer,
                Item::DoNotDisturb,
                Item::Notifications,
                Item::Resume,
                Item::Power
            ]
        );
    }

    #[test]
    fn the_window_entries_are_offered_only_when_a_window_is_selected() {
        let guide = Guide::default();
        assert!(guide.items(WINDOW).contains(&Item::Close));
        assert!(guide.items(WINDOW).contains(&Item::StartScreen));

        // The start screen's card is selected: nothing to kill, and nothing
        // for Start screen to do that Resume does not already do from here.
        assert!(!guide.items(START_CARD).contains(&Item::Close));
        assert!(!guide.items(START_CARD).contains(&Item::StartScreen));

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
        for expected in [Item::Close, Item::StartScreen, Item::Power] {
            assert!(guide.move_selection(1, WINDOW));
            assert_eq!(guide.selected_item(WINDOW), Some(expected));
        }

        // Sitting on the power button when the application exits must not
        // index past the shorter column, nor land on something else.
        assert_eq!(guide.selected_item(START_CARD), Some(Item::Power));
        assert_eq!(guide.selected_index(START_CARD), 4);

        // And on round to the top of the column rather than stopping there,
        // which is the tile line: the mixer is on it and is always a stop.
        assert!(guide.move_selection(1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));
    }

    /// The reason the selection is held as an entry and not a row number:
    /// scrolling the cards onto the start screen drops two rows, and a
    /// remembered row 3 would mean "Start screen" before and "Power" after.
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
        assert_eq!(guide.selected_index(WINDOW), 6);
        assert_eq!(guide.selected_index(START_CARD), 4);
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

        for expected in [
            PowerItem::Shutdown,
            PowerItem::Restart,
            PowerItem::LogOut,
            PowerItem::Cancel,
        ] {
            assert!(guide.move_power(1));
            assert_eq!(guide.power_item(), Some(expected));
        }
        // And Down at Cancel does not carry on round to "Suspend System".
        assert!(!guide.move_power(1));
        assert_eq!(guide.power_item(), Some(PowerItem::Cancel));
    }

    /// Turning the machine off is the only choice drawn as one there is no
    /// coming back from. Restarting and logging out end just as much, and both
    /// of them bring the machine back on their own.
    #[test]
    fn only_turning_the_machine_off_is_drawn_as_grave() {
        let grave: Vec<PowerItem> = POWER_ITEMS
            .iter()
            .copied()
            .filter(|item| item.is_grave())
            .collect();
        assert_eq!(grave, [PowerItem::Shutdown]);
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

    /// The menu steps back while the videos over it have its directions, and
    /// comes forward when it gets them back — both over a span rather than
    /// between two frames, and both from wherever the other one had got to.
    #[test]
    fn the_menu_steps_back_while_a_video_has_its_directions() {
        let frame = 1.0 / 60.0;
        let settle = |guide: &mut Guide, away: bool| {
            for _ in 0..60 {
                guide.animate_elsewhere(away, frame);
            }
        };

        let mut guide = Guide::default();
        guide.open();
        assert_eq!(
            guide.animate_elsewhere(false, frame),
            0.0,
            "the menu has its own directions when it opens"
        );

        let first = guide.animate_elsewhere(true, frame);
        assert!(
            first > 0.0 && first < 1.0,
            "it steps back rather than cutting: {first}"
        );
        settle(&mut guide, true);
        assert_eq!(guide.animate_elsewhere(true, 0.0), 1.0);

        let coming_back = guide.animate_elsewhere(false, frame);
        assert!(
            coming_back > 0.0 && coming_back < 1.0,
            "and comes forward rather than cutting: {coming_back}"
        );
        settle(&mut guide, false);
        assert_eq!(guide.animate_elsewhere(false, 0.0), 0.0);

        // Turned round part-way, it carries on from where it is rather than
        // finishing the movement it was making first.
        guide.animate_elsewhere(true, frame * 3.0);
        let reversed_from = guide.animate_elsewhere(true, 0.0);
        assert!(guide.animate_elsewhere(false, frame) < reversed_from);

        // And a menu put away while a video had the directions opens with them
        // back, rather than opening dim and brightening.
        settle(&mut guide, true);
        guide.open();
        assert_eq!(guide.animate_elsewhere(false, 0.0), 0.0);
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

    /// Alt+Tab's step: it leaves the entry column for the deck without being
    /// asked to, and it comes round rather than stopping at the last card.
    ///
    /// Three cards here, which on this shell is two applications and the start
    /// screen — and the start screen is a card the walk lands on like any
    /// other, because it is where an application is started from.
    #[test]
    fn a_walk_through_the_applications_wraps_round_the_deck() {
        const CARDS: usize = 3;
        let mut guide = Guide::default();
        guide.open();
        assert_eq!(guide.pane(), Pane::Menu);

        // The first step is off the menu and onto the *second* card: card one
        // is the application already in front, and somebody pressing this once
        // is asking for the one behind it.
        assert!(guide.walk(false, CARDS));
        assert_eq!(guide.pane(), Pane::Windows);
        assert_eq!(guide.selected_window(CARDS), 1);

        assert!(guide.walk(false, CARDS));
        assert_eq!(guide.selected_window(CARDS), 2);

        // And round, rather than sitting on the last card. This is where the
        // walk and the directions part company: Down stops here on purpose.
        assert!(guide.walk(false, CARDS));
        assert_eq!(guide.selected_window(CARDS), 0);
        assert!(!guide.move_focus(Move::Up, CARDS));

        // The other way is the same walk backwards, off the end of the deck.
        assert!(guide.walk(true, CARDS));
        assert_eq!(guide.selected_window(CARDS), 2);
    }

    /// A deck of one card is the start screen alone, and there is nothing to
    /// walk to. The shell refuses the press before this — see
    /// `Shell::walk_the_applications` — and the model agrees rather than
    /// reselecting the card the highlight is already on.
    #[test]
    fn a_walk_with_nowhere_to_go_moves_nothing() {
        let mut guide = Guide::default();
        guide.open();
        assert!(!guide.walk(false, 1));
        assert!(!guide.walk(true, 1));
        assert!(!guide.walk(false, 0));
        assert_eq!(guide.pane(), Pane::Menu);
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
        assert_eq!(guide.items(WINDOW).get(3), Some(&Item::Volume));
        assert!(!guide.items(WINDOW).contains(&Item::Brightness));

        guide.set_bars(BOTH_BARS);
        guide.set_pointer_control(true);
        assert_eq!(
            guide.items(WINDOW),
            vec![
                Item::Pointer,
                Item::Mixer,
                Item::DoNotDisturb,
                Item::Notifications,
                Item::Volume,
                Item::Brightness,
                Item::Resume,
                Item::Close,
                Item::StartScreen,
                Item::Power
            ]
        );
        // Losing the window still only takes the window's own entries.
        assert_eq!(
            guide.items(START_CARD),
            vec![
                Item::Pointer,
                Item::Mixer,
                Item::DoNotDisturb,
                Item::Notifications,
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
        // Tiles, bars | Resume, Close | Start screen
        assert_eq!(separator_rows(&guide.items(WINDOW)), vec![6, 8]);
        // The same, with the window's own two entries gone.
        assert_eq!(separator_rows(&guide.items(START_CARD)), vec![6]);

        // No rule between the tiles and the bars: they are the same band —
        // what the session sounds and looks and behaves like. With neither bar
        // the three panel tiles are that whole band on their own, and the rule
        // under them is the one that was there before the tiles were.
        let plain = Guide::default();
        assert_eq!(separator_rows(&plain.items(WINDOW)), vec![3, 5]);
        assert_eq!(separator_rows(&plain.items(START_CARD)), vec![3]);
    }

    /// A bar is slid rather than pressed, and the two are told apart by the
    /// entry itself so that no caller has to keep a list. The tiles are told
    /// apart the same way, and are neither bars nor rows.
    #[test]
    fn every_entry_knows_which_kind_of_control_it_is() {
        assert_eq!(Item::Volume.bar(), Some(Bar::Volume));
        assert_eq!(Item::Brightness.bar(), Some(Bar::Brightness));
        for button in [Item::Resume, Item::Close, Item::StartScreen, Item::Power] {
            assert_eq!(button.bar(), None, "{button:?}");
            assert!(!button.is_tile(), "{button:?}");
            assert!(!button.label(Some("Celeste")).is_empty() || button == Item::Power);
        }
        // Neither bar carries a label: the track is the whole control.
        assert!(Item::Volume.label(None).is_empty());
        assert!(Item::Brightness.label(None).is_empty());

        // The tiles are switches: a glyph, no track, no label, and neither of
        // them is a bar that Left and Right would slide.
        for tile in [Item::Pointer, Item::Mixer, Item::Notifications] {
            assert!(tile.is_tile(), "{tile:?}");
            assert_eq!(tile.bar(), None, "{tile:?}");
            assert!(tile.label(Some("Celeste")).is_empty(), "{tile:?}");
            assert!(tile.glyph().is_some(), "{tile:?}");
        }
        assert!(Item::Volume.glyph().is_none());
    }

    /// The tiles share one line, and everything else has one to itself.
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
            vec![(0, 4), (4, 1), (5, 1), (6, 1), (7, 1), (8, 1), (9, 1)]
        );

        // A session with no stick pointer leaves three on the line rather than
        // breaking it up.
        guide.set_pointer_control(false);
        let items = guide.items(START_CARD);
        assert_eq!(lines(&items), vec![(0, 3), (3, 1), (4, 1), (5, 1), (6, 1)]);
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

        // With an application in front every tile can be reached, so the line
        // is four stops and Right walks them.
        assert!(guide.can_move_in_line(1, WINDOW));
        assert!(guide.move_in_line(1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));
        assert!(guide.move_in_line(1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::DoNotDisturb));
        assert!(guide.move_in_line(1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Notifications));
        // Off the last one is where the caller learns to cross to the cards.
        assert!(!guide.can_move_in_line(1, WINDOW));

        // Without an application the switch is not a stop, so Left from the
        // mixer steps over it and leaves the column rather than landing on a
        // control that does nothing.
        guide.move_in_line(-1, WINDOW);
        guide.move_in_line(-1, WINDOW);
        guide.set_pointer_target(false);
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));
        assert!(!guide.can_move_in_line(-1, WINDOW));
        assert!(!guide.move_in_line(-1, WINDOW));
        assert_eq!(guide.selected_item(WINDOW), Some(Item::Mixer));

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
        // it is making whether or not anything is running, and the switch
        // beside it is about whether the session may be interrupted, which is
        // a question with an answer on an empty machine.
        assert!(!guide.is_enabled(Item::Pointer));
        assert!(guide.is_enabled(Item::Mixer));
        assert!(guide.is_enabled(Item::DoNotDisturb));
        guide.set_pointer_target(true);
        assert!(guide.is_enabled(Item::Pointer));
        assert!(guide.is_enabled(Item::Mixer));
        assert!(guide.is_enabled(Item::DoNotDisturb));

        // Everything else in the column always does something.
        for item in [
            Item::Notifications,
            Item::Volume,
            Item::Brightness,
            Item::Resume,
            Item::Close,
            Item::StartScreen,
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
        assert_eq!(guide.selected_index(WINDOW), 4);
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
        assert_eq!(Item::StartScreen.label(None), "Start screen");
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
    /// A menu put away without the user choosing a row still has to leave the
    /// bar in the right place, and the two answers are not interchangeable.
    ///
    /// Getting it backwards is visible immediately: the bar dropped below an
    /// application that is still in front of it, or the bar holding the
    /// overlay and the keyboard over a display with nothing on it.
    #[test]
    fn putting_the_menu_away_leaves_the_bar_where_the_display_needs_it() {
        let mut guide = Guide::default();
        guide.open();
        assert!(guide.is_menu());

        // Nothing running there: the bar comes back plainly, and stops being
        // drawn over anything.
        guide.dismiss(false);
        assert_eq!(guide.mode(), Mode::Bar);
        assert!(!guide.is_over_app());

        // Something running there: the bar stays over it.
        guide.open();
        guide.dismiss(true);
        assert_eq!(guide.mode(), Mode::BarOverApp);
        assert!(guide.is_over_app());
    }

    /// The power dialog is part of the menu and goes away with it.
    ///
    /// It is drawn above everything else in the guide, so a dialog that
    /// outlived the dismissal would be the one thing left on screen — and it
    /// is the one panel in the shell where a stray `A` turns the machine off.
    #[test]
    fn putting_the_menu_away_takes_its_power_dialog_with_it() {
        for app_running in [false, true] {
            let mut guide = Guide::default();
            guide.open();
            guide.open_power();
            assert!(guide.power_open());

            guide.dismiss(app_running);
            assert!(
                !guide.power_open(),
                "the dialog survived being dismissed with app_running={app_running}"
            );
        }
    }

    /// A launch splash has to be *above* the window it is waiting for — that
    /// is the whole trick, the application maps underneath and is revealed
    /// rather than appearing on top of the shell. And it must not hold the
    /// keyboard, or the application would come up unable to be typed at.
    #[test]
    fn a_launch_splash_waits_above_the_window_it_is_waiting_for() {
        let guide = Guide::default();
        for driven in [true, false] {
            assert_eq!(
                guide.surface_state(
                    driven,
                    true,
                    false,
                    true,
                    false,
                    false,
                    false,
                    false,
                    false,
                    Layer::Background
                ),
                (Layer::Overlay, KeyboardInteractivity::None),
                "the splash is on the display it was started from, driven or not"
            );
        }
        // And the display goes straight back to where it was afterwards.
        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::OnDemand)
        );
    }

    #[test]
    fn only_the_driven_display_rises_for_the_guide() {
        let mut guide = Guide::default();
        guide.open();

        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::Exclusive)
        );
        for mode in [Mode::Menu, Mode::BarOverApp] {
            if mode == Mode::BarOverApp {
                guide.show_start_screen_over_app();
            }
            assert_eq!(
                guide.surface_state(
                    false,
                    true,
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    Layer::Background
                ),
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
                Mode::BarOverApp => guide.show_start_screen_over_app(),
            }
            for app_running in [false, true] {
                for keep_grabbed in [false, true] {
                    let (_, interactivity) = guide.surface_state(
                        false,
                        app_running,
                        keep_grabbed,
                        false,
                        false,
                        false,
                        false,
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
            guide.surface_state(
                true,
                true,
                false,
                false,
                true,
                true,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::None)
        );
        // Even when the shell was told to hold the keyboard regardless: the
        // debugging flag cannot be allowed to make the keyboard useless.
        assert_eq!(
            guide.surface_state(
                true,
                true,
                true,
                false,
                true,
                true,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::None)
        );
        // Not on displays nobody is driving.
        assert_eq!(
            guide.surface_state(
                false,
                true,
                false,
                false,
                true,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::None)
        );
        // And the menu wins if both somehow claim the display, because the
        // menu is the one that needs the keys.
        let mut guide = Guide::default();
        guide.open();
        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                true,
                true,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::Exclusive)
        );
    }

    /// A board pinned to a display of its own — Settings > Input > On-screen
    /// keyboard > Default display — lifts *that* display and no other.
    ///
    /// The two halves come apart here, and each has to land on the right
    /// screen. The **layer** belongs to the display the keys are drawn on: a
    /// surface left behind its application there would paint the board under
    /// the very window it was raised for, and one lifted on the driven display
    /// instead would put the whole start screen over whatever is in front
    /// *there*. The **keys** belong to neither display in particular — there is
    /// one seat, and whatever it points at is what the board types into — so
    /// they are given up on the driven screen exactly as they always were.
    #[test]
    fn a_board_pinned_to_a_screen_lifts_that_screen_and_leaves_the_others() {
        let guide = Guide::default();

        // The screen the board is on, which nobody is driving. Lifted over its
        // application, and taking nothing from it.
        assert_eq!(
            guide.surface_state(
                false,
                true,
                false,
                false,
                true,
                true,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::None)
        );
        // The screen being driven, with the board somewhere else: it keeps its
        // own layer — there is nothing of the board's to lift it for — and
        // still hands the keys over, because a shell holding them would read
        // every letter the board types as a direction on the bar.
        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                true,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::None)
        );
        // Unless the board is typing into a field of the shell's own, which is
        // drawn by the driven display whatever screen the keys are on. That
        // display keeps both the lift and the keys.
        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                true,
                false,
                true,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::Exclusive)
        );
        // And the screen the keys are on takes no keys even then: two surfaces
        // claiming the seat is one of them losing, and the one that must not
        // lose is the one holding the field.
        assert_eq!(
            guide.surface_state(
                false,
                true,
                false,
                false,
                true,
                true,
                true,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::None)
        );
    }

    /// The volume the keys raise is the other thing drawn over an application
    /// without anything being opened, and it lives or dies by this: the whole
    /// point of the keys is that they work while a game holds the display, so
    /// a control left on the layer under that game is a key that does nothing.
    ///
    /// And it must hand back what it rose over. The hand that pressed the key
    /// is on the keyboard of a game still being played — a control that took
    /// the keys for a second would be a second of that game played by nobody.
    #[test]
    fn the_volume_keys_lift_the_shell_over_the_application_and_take_nothing() {
        let guide = Guide::default();
        const RAISED: bool = true;

        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                RAISED,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::None),
            "the control is over the game, and holds neither its keys nor its clicks"
        );

        // The same three states a bubble may not change, for the same reasons:
        // the shell holding the keys on purpose, and a display nobody is
        // driving.
        assert_eq!(
            guide.surface_state(
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                RAISED,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::Exclusive),
            "with nothing running the bar is already the top of the display"
        );
        assert_eq!(
            guide.surface_state(
                true,
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                RAISED,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::Exclusive),
            "a volume key must not answer a dialog by taking the keys off it"
        );
        assert_eq!(
            guide.surface_state(
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                RAISED,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::None),
            "and a display nobody is driving stays where it is"
        );
    }

    /// A bubble in the corner is the one thing the shell draws over an
    /// application without being asked, so it has to reach the overlay layer —
    /// and, reaching it, has to hand back both the keys and the clicks, or a
    /// four-second announcement makes the game under it unplayable.
    ///
    /// It is exactly the launch splash's state, which is what
    /// `passes_pointer_through` in the shell reads to make the surface
    /// click-through. Any other pair here would be a transparent sheet over
    /// the whole display swallowing every press.
    #[test]
    fn a_bubble_lifts_the_shell_over_the_application_and_keeps_its_hands_off_it() {
        let guide = Guide::default();
        const TOASTING: bool = true;

        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::OnDemand),
            "with nothing announced the bar stays where it was"
        );
        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                TOASTING,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::None),
            "and rises for one, taking neither the keys nor the pointer"
        );
    }

    /// The three states a bubble must *not* change, each for its own reason:
    /// two of them are already over the application, and the third is the
    /// shell holding the keyboard because it is what the user is using.
    #[test]
    fn a_bubble_never_takes_the_keyboard_off_something_that_needs_it() {
        // Nothing running: this is the start screen, and the shell holds the
        // keys outright. An announcement arriving must not take them — the
        // corner is drawn on the shell's own surface, which is already the
        // topmost thing on the display.
        let guide = Guide::default();
        assert_eq!(
            guide.surface_state(
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                true,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::Exclusive)
        );

        // The shell deliberately holding the keyboard over an application — a
        // dialog, a question from outside the session. A bubble must not
        // quietly answer it by taking the keys away.
        assert_eq!(
            guide.surface_state(
                true,
                true,
                true,
                false,
                false,
                false,
                false,
                true,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::Exclusive)
        );

        // The guide, which is over the application already and needs the keys
        // for its own column.
        let mut open = Guide::default();
        open.open();
        assert_eq!(
            open.surface_state(
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                true,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::Exclusive)
        );

        // And a display nobody is driving stays put whatever the corner of the
        // driven one is doing. The shell only ever passes `toasting` for the
        // focused display, but the answer must not depend on it remembering.
        assert_eq!(
            guide.surface_state(
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                true,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::None)
        );
    }

    /// The one board that does take the keys: the one typing into the shell.
    ///
    /// Nothing out there has a text field for it to deactivate, and the shell
    /// needs them held *through* the board rather than handed back when it
    /// goes — the board holds the physical keyboard itself, and the key that
    /// dismisses it lands in the gap left by a surface that had given up
    /// focus.
    #[test]
    fn a_board_typing_into_the_shell_keeps_the_keys_it_is_typing_with() {
        let guide = Guide::default();
        for app_running in [false, true] {
            assert_eq!(
                guide.surface_state(
                    true,
                    app_running,
                    false,
                    false,
                    true,
                    true,
                    true,
                    false,
                    false,
                    Layer::Background
                ),
                (Layer::Overlay, KeyboardInteractivity::Exclusive),
                "app_running={app_running}: the field is the shell's, so the keys are too"
            );
        }
        // Still only on the display being driven, and still nothing at all
        // while a launch is on screen: neither of those is about where the
        // letters are going.
        assert_eq!(
            guide.surface_state(
                false,
                true,
                false,
                false,
                true,
                false,
                true,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::None)
        );
        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                true,
                true,
                true,
                true,
                false,
                false,
                Layer::Background
            ),
            (Layer::Overlay, KeyboardInteractivity::None)
        );
    }

    #[test]
    fn the_bar_yields_the_keyboard_to_a_running_application() {
        let guide = Guide::default();
        assert_eq!(
            guide.surface_state(
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::Exclusive)
        );
        assert_eq!(
            guide.surface_state(
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::OnDemand)
        );
        // Unless it was told not to.
        assert_eq!(
            guide.surface_state(
                true,
                true,
                true,
                false,
                false,
                false,
                false,
                false,
                false,
                Layer::Background
            ),
            (Layer::Background, KeyboardInteractivity::Exclusive)
        );
    }

    #[test]
    fn overlay_modes_are_the_ones_drawn_above_an_application() {
        let mut guide = Guide::default();
        assert!(!guide.is_over_app());
        guide.open();
        assert!(guide.is_over_app());
        guide.show_start_screen_over_app();
        assert!(guide.is_over_app());
        guide.close();
        assert!(!guide.is_over_app());
    }

    fn now(title: &str) -> NowPlaying {
        NowPlaying {
            bus: "org.mpris.MediaPlayer2.fixture".to_string(),
            title: title.to_string(),
            app: "Fixture".to_string(),
            playing: true,
            can_previous: true,
            can_next: true,
            level: Some(crate::system::Level {
                value: 0.5,
                muted: false,
            }),
            stream: Some(7),
        }
    }

    /// The whole of what the user asked for: the card is not there when
    /// nothing is playing, it is when something is, and it goes away again on
    /// its own once the music has been let go of.
    #[test]
    fn the_media_rows_come_and_go_with_what_is_playing() {
        let mut guide = Guide::default();
        assert!(!guide.items(false).iter().any(|item| item.is_media()));

        guide.set_now_playing(Some(now("A Track")));
        let items = guide.items(false);
        assert!(items.contains(&Item::Media));
        assert!(items.contains(&Item::MediaVolume));
        // The groove sits directly under the session's own, and the card
        // after every bar there is.
        let at = |wanted: Item| items.iter().position(|item| *item == wanted);
        assert!(at(Item::MediaVolume) < at(Item::Media));
        assert!(at(Item::Media) < at(Item::Resume));
    }

    /// And below the machine's own bars, not through the middle of them: a
    /// control that comes and goes must not separate two that never do.
    #[test]
    fn the_media_rows_sit_under_every_bar_the_machine_has() {
        let mut guide = Guide::default();
        guide.set_bars(Bars {
            volume: true,
            brightness: true,
        });
        guide.set_now_playing(Some(now("A Track")));
        let items = guide.items(false);
        let at = |wanted: Item| items.iter().position(|item| *item == wanted);
        assert!(at(Item::Volume) < at(Item::Brightness));
        assert!(at(Item::Brightness) < at(Item::MediaVolume));
        assert!(at(Item::MediaVolume) < at(Item::Media));
    }

    /// Nothing disappears before its transition finishes: the card is still in
    /// the column, and still carrying what to draw, for the whole of its way
    /// out.
    #[test]
    fn the_card_is_still_there_while_it_is_closing() {
        let mut guide = Guide::default();
        guide.set_now_playing(Some(now("A Track")));
        while guide.animate_media(0.05) < 1.0 {}

        guide.set_now_playing(None);
        let mut frames = 0;
        while guide.media() > 0.0 {
            assert!(
                guide.has_media() && guide.now_playing().is_some(),
                "the card stopped being drawable while it was still on screen"
            );
            guide.animate_media(0.05);
            frames += 1;
            assert!(frames < 200, "the card never finished closing");
        }
        // And only then is it forgotten.
        assert!(!guide.has_media());
        assert!(guide.now_playing().is_none());
        assert!(!guide.items(false).iter().any(|item| item.is_media()));
    }

    /// A card taken away while it is still arriving falls back from where it
    /// got to rather than snapping open first.
    #[test]
    fn a_card_taken_away_half_way_in_closes_from_there() {
        let mut guide = Guide::default();
        guide.set_now_playing(Some(now("A Track")));
        guide.animate_media(MEDIA_FLIGHT * 0.5);
        let half = guide.media();
        assert!(half > 0.2 && half < 0.8, "half way in, not {half}");

        guide.set_now_playing(None);
        let next = guide.animate_media(MEDIA_FLIGHT * 0.1);
        assert!(next < half, "it went on opening: {half} then {next}");
    }

    /// A track changing must not restart the way in, nor move the selection
    /// off the button under the user's thumb.
    #[test]
    fn a_new_track_does_not_reopen_the_card() {
        let mut guide = Guide::default();
        guide.set_now_playing(Some(now("First")));
        while guide.animate_media(0.05) < 1.0 {}
        guide.select(Item::Media, false);
        guide.move_transport(1, false);

        guide.set_now_playing(Some(now("Second")));
        assert_eq!(guide.media(), 1.0);
        assert_eq!(guide.transport(), Transport::Next);
        assert_eq!(guide.now_playing().map(|now| now.line()), Some("Second"));
    }

    /// Left and Right walk the three buttons and stop at the ends, where they
    /// go back to meaning what they mean everywhere else in the column.
    #[test]
    fn the_transport_is_walked_and_stops_at_both_ends() {
        let mut guide = Guide::default();
        guide.set_now_playing(Some(now("A Track")));
        guide.select(Item::Media, false);
        assert_eq!(guide.transport(), Transport::PlayPause);

        assert!(guide.move_transport(-1, false));
        assert_eq!(guide.transport(), Transport::Previous);
        assert!(!guide.can_move_transport(-1, false));
        assert!(!guide.move_transport(-1, false));

        assert!(guide.move_transport(1, false));
        assert!(guide.move_transport(1, false));
        assert_eq!(guide.transport(), Transport::Next);
        assert!(!guide.can_move_transport(1, false));
    }

    /// A button the player will not answer is stepped over, not stopped on —
    /// the same walk the tile line makes past a tile that cannot be reached.
    /// It matters beyond the press: the selection is the one thing that changes
    /// a transport button's colour, so a highlight able to rest on a dead
    /// button would light it.
    #[test]
    fn the_selection_steps_over_a_button_the_player_will_not_answer() {
        let mut guide = Guide::default();
        let mut ends = now("The Last One");
        ends.can_next = false;
        guide.set_now_playing(Some(ends));
        guide.select(Item::Media, false);

        // Right has nowhere to go: the only button that way is dead, so the
        // press means what it means everywhere else in the column.
        assert!(!guide.can_move_transport(1, false));
        assert!(!guide.move_transport(1, false));
        assert_eq!(guide.transport(), Transport::PlayPause);

        // Left still walks, because that one can be pressed.
        assert!(guide.move_transport(-1, false));
        assert_eq!(guide.transport(), Transport::Previous);
        // And back the other way it steps over the dead one rather than
        // stopping on it.
        assert!(guide.move_transport(1, false));
        assert_eq!(guide.transport(), Transport::PlayPause);
        assert!(!guide.can_move_transport(1, false));
    }

    /// And a pointer may not put it there either.
    #[test]
    fn a_pointer_cannot_choose_a_button_the_player_will_not_answer() {
        let mut guide = Guide::default();
        let mut ends = now("The Last One");
        ends.can_next = false;
        guide.set_now_playing(Some(ends));
        assert!(!guide.select_transport(Transport::Next, false));
        assert_eq!(guide.transport(), Transport::PlayPause);
        assert!(guide.select_transport(Transport::Previous, false));
        assert_eq!(guide.transport(), Transport::Previous);
    }

    /// A button that goes dead *under* the highlight takes it back to play,
    /// which is the one button that is always there — the same fall-back the
    /// column makes to Resume when an entry disappears.
    #[test]
    fn a_button_that_dies_under_the_highlight_gives_it_up() {
        let mut guide = Guide::default();
        guide.set_now_playing(Some(now("A Track")));
        guide.select(Item::Media, false);
        assert!(guide.move_transport(1, false));
        assert_eq!(guide.transport(), Transport::Next);

        let mut ends = now("The Last One");
        ends.can_next = false;
        guide.set_now_playing(Some(ends));
        assert_eq!(guide.transport(), Transport::PlayPause);
    }

    /// And they mean nothing at all while the highlight is on some other row,
    /// or the volume bar above the card could never be slid.
    #[test]
    fn the_transport_is_only_walked_from_the_card() {
        let mut guide = Guide::default();
        guide.set_now_playing(Some(now("A Track")));
        guide.select(Item::MediaVolume, false);
        assert!(!guide.can_move_transport(1, false));
        assert!(!guide.move_transport(1, false));
    }

    /// The selection glides between the buttons rather than jumping.
    #[test]
    fn the_selection_travels_between_the_buttons() {
        let mut guide = Guide::default();
        guide.set_now_playing(Some(now("A Track")));
        guide.select(Item::Media, false);
        while guide.animate_media(0.05) < 1.0 {}
        assert_eq!(guide.transport_at(), 1.0);

        guide.move_transport(1, false);
        guide.animate_media(TRANSPORT_FLIGHT * 0.5);
        let midway = guide.transport_at();
        assert!(midway > 1.0 && midway < 2.0, "jumped straight to {midway}");
        while guide.animate_media(0.05) < 1.0 || guide.transport_at() < 2.0 {}
        assert_eq!(guide.transport_at(), 2.0);
    }

    /// A player that says nothing about what is in it is still legible: the
    /// card falls back to the application's own name rather than printing a
    /// blank line under three buttons.
    #[test]
    fn a_card_with_no_title_says_what_the_application_is() {
        let mut nothing = now("");
        assert_eq!(nothing.line(), "Fixture");
        nothing.title = "Something".to_string();
        assert_eq!(nothing.line(), "Something");
    }

    /// The groove is left out where the sound server has nothing of the
    /// application in it, the way a machine with no backlight leaves out the
    /// brightness bar — but the card stays, because there is still something
    /// to press.
    #[test]
    fn the_groove_needs_a_stream_and_the_card_does_not() {
        let mut guide = Guide::default();
        let mut silent = now("A Track");
        silent.level = None;
        guide.set_now_playing(Some(silent));
        let items = guide.items(false);
        assert!(items.contains(&Item::Media));
        assert!(!items.contains(&Item::MediaVolume));
    }

    /// A download, for the card in the corner.
    fn coming(app_id: u32, name: &str, share: Option<f32>) -> crate::steam::Coming {
        crate::steam::Coming {
            app_id,
            name: name.to_string(),
            verb: "Downloading",
            share,
            stuck: false,
            a_download: true,
        }
    }

    /// The card comes and goes with the download, on its own flight, and goes
    /// on saying what it said until it has finished leaving.
    ///
    /// The last part is the shell's motion rule and the whole reason what the
    /// card is about is kept here rather than read from the library every
    /// frame: a download that has finished is gone from the library at once,
    /// and a card that lost its name a third of a second before it left the
    /// screen would be an empty panel sliding off.
    #[test]
    fn the_download_card_keeps_its_words_until_it_has_finished_leaving() {
        let mut guide = Guide::default();
        assert_eq!(guide.download(), 0.0);
        assert!(guide.downloading().is_none());

        guide.set_downloading(Some(coming(945360, "Among Us", Some(0.33))));
        assert!(guide.download_is_moving());
        while guide.animate_download(0.05) < 1.0 {}
        assert!(!guide.download_is_moving());
        assert_eq!(
            guide.downloading().map(|coming| coming.said()),
            Some("Downloading Among Us".to_string())
        );

        guide.set_downloading(None);
        while guide.download() > 0.0 {
            assert!(
                guide.downloading().is_some(),
                "the card must keep its name for the whole of the way out"
            );
            guide.animate_download(0.05);
        }
        assert!(guide.downloading().is_none());
        assert!(!guide.download_is_moving());
    }

    /// A percentage arriving does not restart the way in.
    ///
    /// Valve's client says how far it has got every couple of seconds, and a
    /// card that flew in again at each of them would be a card nobody could
    /// read. The same rule the media card is under when a track changes.
    #[test]
    fn a_new_percentage_does_not_send_the_card_back_out() {
        let mut guide = Guide::default();
        guide.set_downloading(Some(coming(945360, "Among Us", Some(0.10))));
        guide.animate_download(ARRIVING_FLIGHT * 0.5);
        let half = guide.download();
        assert!(half > 0.0 && half < 1.0);

        guide.set_downloading(Some(coming(945360, "Among Us", Some(0.20))));
        assert_eq!(guide.download(), half, "it carries on from where it was");
        assert_eq!(
            guide.downloading().and_then(|coming| coming.share),
            Some(0.20),
            "and says the new number"
        );
    }

    /// The two corner cards are two things happening, and neither takes the
    /// other's place.
    ///
    /// A machine can be installing its updates while a game comes down — the
    /// update panel says as much, since it pauses sleep and shutdown and not
    /// Steam — and a corner that showed one of them would be silent about the
    /// other at the moment somebody opened the menu to look. So they stack:
    /// the download keeps the corner it was drawn for, the machine's own work
    /// stands on top of it, and when the download has finished leaving the
    /// card above comes *down* into the corner rather than being found there
    /// on the next frame.
    #[test]
    fn the_machines_own_work_stands_on_top_of_a_download_and_comes_down_after_it() {
        let mut guide = Guide::default();
        let updating = Working {
            said: "Updating System".into(),
            share: Some(0.4),
        };
        // Both are advanced together every frame, as the loop advances them.
        fn frame(guide: &mut Guide, dt: f32) {
            guide.animate_download(dt);
            guide.animate_working(dt);
        }
        guide.set_working(Some(updating.clone()));
        while guide.working_is_moving() {
            frame(&mut guide, 0.05);
        }
        assert_eq!(guide.working_at(), 1.0);
        assert_eq!(guide.working_row(), 0.0, "alone, it is the corner");

        // A download starting under it lifts it, and it is seen to be lifted.
        guide.set_downloading(Some(coming(945360, "Among Us", Some(0.33))));
        frame(&mut guide, ARRIVING_FLIGHT * 0.5);
        let midway = guide.working_row();
        assert!(midway > 0.0 && midway < 1.0, "jumped straight to {midway}");
        while guide.working_is_moving() || guide.download_is_moving() {
            frame(&mut guide, 0.05);
        }
        assert_eq!(guide.working_row(), 1.0);

        // The download finishing brings it back down — and not before the card
        // under it has finished leaving, or it would come down onto one.
        guide.set_downloading(None);
        while guide.downloading().is_some() {
            assert_eq!(guide.working_row(), 1.0, "not while there is one under it");
            frame(&mut guide, 0.05);
        }
        while guide.working_is_moving() {
            frame(&mut guide, 0.05);
        }
        assert_eq!(guide.working_row(), 0.0);

        // And it keeps its words for the whole of its own way out, exactly as
        // the card beside it does.
        guide.set_working(None);
        while guide.working_at() > 0.0 {
            assert!(guide.working().is_some());
            frame(&mut guide, 0.05);
        }
        assert!(guide.working().is_none());
        assert!(!guide.working_is_moving());
    }

    /// A download that finishes while its card is still arriving leaves from
    /// where it got to rather than snapping open first — one linear position,
    /// reversible wherever it is, like every other flight in this menu.
    #[test]
    fn a_card_turned_round_half_way_carries_on_from_where_it_is() {
        let mut guide = Guide::default();
        guide.set_downloading(Some(coming(504230, "Celeste", None)));
        guide.animate_download(ARRIVING_FLIGHT * 0.4);
        let reached = guide.download();
        guide.set_downloading(None);
        let next = guide.animate_download(ARRIVING_FLIGHT * 0.1);
        assert!(next < reached, "it starts back from where it had got to");
        assert!(next > 0.0, "and does not jump to the end");
    }
}
