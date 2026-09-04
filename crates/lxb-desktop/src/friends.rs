//! The friends panel: who is on Steam, on a column that slides in from the
//! right.
//!
//! The guide's sidebar is on the left because it is *this session* — the hour,
//! the machine, what is running, the way out. This one is on the right because
//! it is everybody else, and because the two are never both being read: the
//! panel takes the directions the moment it is up, so a screen showing both is
//! a screen where one of them is waiting.
//!
//! ## What this module is, and what it is not
//!
//! It is where the user is standing, and nothing else. What the rows *say* is
//! [`lxb_steam::Roster`], which arrives from Steam and is replaced wholesale
//! whenever anybody in it moves — so nothing here may be a copy of it. A panel
//! that held its own list would be showing where somebody was a minute ago, and
//! a selection kept as a pointer into that list would be pointing at a
//! different person by the time it was pressed.
//!
//! The position used to be only an index. That makes a presence update which
//! moves somebody between bands silently move the selection to a different
//! person. The index is still what drawing needs, but the identity remembered
//! across roster replacements is the SteamID; [`Friends::settle`] resolves it
//! back to an index after every update.
//!
//! ## The shape of the list
//!
//! Three bands, in the order the user asked for: people in a game, people who
//! are around, and people who are not. Each band is a rule with its own name
//! and a count, and only the rows between them can be stood on — see [`Line`],
//! which is the one flattening both the drawing and the walking are done
//! against, so a heading can never be selected and a scroll can never stop
//! half way through one.

use lxb_steam::chat::{Conversation, Mark};
use lxb_steam::{Band, Roster};

/// How long the panel takes to come in from the edge, and to go back out.
///
/// A little longer than the guide's own sidebar ([`crate::ui::GUIDE_SLIDE`] is
/// 0.28), because this one travels the same distance over a screen that is
/// already full: the guide slides in over a wallpaper it dimmed first, and this
/// slides in over whatever the user was reading. Long enough to be followed by
/// an eye that was not looking at the right edge when the button went down.
///
/// Never linear — see [`crate::ui::ease`], which every reading of this goes
/// through.
pub const SLIDE: f32 = 0.32;

/// How near the list has to be to where it is going before it is called
/// arrived: a hundredth of a line, and a hundredth of a line per second.
///
/// A critically damped spring approaches its target and never reaches it, and
/// "still moving" is what asks for the next frame — so without this the panel
/// would redraw sixty times a second for the rest of the session over a
/// distance no screen can show.
const SETTLED: f32 = 0.01;

/// How long the panel takes to turn from the list to a conversation, and back.
///
/// Shorter than [`SLIDE`], because nothing is arriving: both halves are already
/// on the panel and one is stepping aside for the other. Long enough to be seen
/// as a movement rather than a cut, which is the standing rule — nothing in
/// this shell vanishes mid-transition, and a conversation that simply replaced
/// the list would be exactly that.
pub const TURN: f32 = 0.22;

/// One line of the panel, in the order it is drawn.
///
/// The flattening of a [`Roster`] into rules and rows, done once and used by
/// the layout, the scroll and the walk — so there is exactly one answer to
/// "what is the fourth thing down the panel", and a heading cannot be stood on
/// because it is not a [`Line::Person`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// A rule with a band's name and how many people are under it.
    Heading(Band, usize),
    /// Somebody, by their place in [`Roster::friends`].
    Person(usize),
}

impl Line {
    /// Which friend this line is, if it is one.
    pub fn person(self) -> Option<usize> {
        match self {
            Line::Person(index) => Some(index),
            Line::Heading(..) => None,
        }
    }
}

/// The panel as a list of lines: a rule above each band, and its people under
/// it.
///
/// A band with nobody in it gets no rule. Steam's own list draws the empty ones
/// as headings with a zero beside them; this does not, because the whole panel
/// is a column of forty rows on a screen watched from a couch and three rules
/// saying nothing are three rows of nothing to scroll past.
///
/// The roster arrives already in this order — see [`lxb_steam::friends`], where
/// the sort is — so this walks it once and never sorts.
pub fn lines(roster: &Roster) -> Vec<Line> {
    let mut lines = Vec::with_capacity(roster.friends.len() + 3);
    let counts = roster.counts();
    let mut band: Option<Band> = None;
    for (index, friend) in roster.friends.iter().enumerate() {
        let theirs = friend.band();
        if band != Some(theirs) {
            lines.push(Line::Heading(theirs, counts[theirs as usize]));
            band = Some(theirs);
        }
        lines.push(Line::Person(index));
    }
    lines
}

/// Where the light is inside an open conversation.
///
/// **Never an index into the message column.** Messages arrive under the reader
/// at any moment — a friend writing, a history landing, a send being confirmed
/// — and an index would be pointing at a different message a frame later, which
/// is the same defect the roster's own selection had. A [`Mark`] is Steam's own
/// key for a message, or the request one is going out under, and it survives
/// every one of those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Talking {
    /// The field at the foot of the panel, which is where a conversation opens.
    /// The user pressed a name to say something.
    Compose,
    /// One message in the column.
    Message(Mark),
    /// The "Try again" a failed history offers, which stands above the column
    /// because it is about the whole of it.
    Again,
}

/// Where the user is standing in the panel, and how far it has arrived.
#[derive(Debug)]
pub struct Friends {
    /// Whether it is taking the directions. Not the same question as whether it
    /// is on screen: a panel that has been dismissed hands the keys straight
    /// back while it is still sliding out.
    open: bool,
    /// How far in it is: 0 gone, 1 settled. A position rather than the moment
    /// it opened, so that the two directions are one movement — a panel
    /// dismissed halfway in goes no further first.
    linear: f32,
    /// Which friend the highlight is on, as a place in [`Roster::friends`].
    ///
    /// Never trusted on its own: the list it indexes is replaced whenever
    /// anybody moves. See [`Self::selected`].
    selected: usize,
    /// Who the selected row belongs to across roster reordering.
    selected_id: Option<u64>,
    /// Whether the light is on the head's status button rather than on a row.
    ///
    /// The one thing on this panel that is not a person. It stands above the
    /// list — Up off the first row reaches it and Down off it returns to the
    /// first row — and it is never where the panel opens: the user pressed a
    /// button to see who is online, and a panel that greeted them with their
    /// own status lit would have answered a question nobody asked.
    ///
    /// Never trusted on its own either, on exactly the terms [`Self::selected`]
    /// is not: the button is only there while somebody is signed in and Steam
    /// is switched on, and both can stop being true under a light that is
    /// already on it. See [`Self::on_status`], which is where that is masked.
    on_status: bool,
    /// Which line is at the top of the body, as a *fraction* of a line.
    ///
    /// The whole of what makes the list scroll rather than step. An integer
    /// first-row — which is what the context menu keeps, and what this kept at
    /// first — moves the entire column by a row's height on the frame the
    /// selection leaves the window, and there is no amount of easing anywhere
    /// else that can hide it: the content is simply somewhere else on the next
    /// frame. So the position is continuous and rides a spring towards
    /// [`Self::wanted`], and every row is laid out against it.
    ///
    /// The same model the bar's own columns and the file panel's use, and the
    /// one the toolkit reaches for — see `level.position` in its picker, where
    /// rows are drawn at `row - position` exactly as they are here.
    scroll: f32,
    scroll_speed: f32,
    /// The line the top of the body is heading for, in whole lines. What
    /// [`Self::keep_the_selection_in_view`] decides, and what the spring above
    /// chases.
    wanted: usize,
    /// How many lines fit, told to the panel by whoever knows how big the
    /// screen is. Zero means nobody has said yet, which is treated as "they all
    /// fit" — the same convention [`crate::menu::Menu`] uses.
    window: usize,
    /// The highlight's glide down the column: it slides from row to row rather
    /// than jumping, on the same spring the guide's and the context menu's
    /// ride. See [`crate::menu::Glide`], which is the spring and the whole of
    /// the rule.
    highlight: crate::menu::Glide,

    // -- the conversation, when one is open ---------------------------------
    /// Whose conversation is open, **by SteamID**.
    ///
    /// Not an index and not a copy of the person: the roster is replaced
    /// whenever anybody in it moves, and a conversation held by position would
    /// be a conversation that changed hands when a friend started a game. The
    /// row it is drawn from is looked up by this id on every frame.
    talking_to: Option<u64>,
    /// How far the panel has turned from the list to the conversation: 0 the
    /// list, 1 the conversation. A position rather than a moment, so a turn
    /// reversed halfway goes back from where it is.
    turned: f32,
    /// Where the light is in the conversation. See [`Talking`].
    talking: Talking,
    /// What has been typed and not sent, for the conversation that is open.
    ///
    /// Kept only while one is: leaving a conversation gives the draft up, which
    /// is the same thing every other field in this shell does when its panel is
    /// dismissed. It is never written down anywhere.
    draft: String,
    /// Whether the field is being typed into, which is what takes the keyboard
    /// and puts the board up.
    composing: bool,
    /// How far down the message column the view is, in **pixels**.
    ///
    /// Pixels rather than lines because a message is as tall as its words are
    /// long: a column counted in rows would say a conversation of one-word
    /// replies and one of paragraphs were the same length.
    chat_scroll: f32,
    chat_speed: f32,
    chat_wanted: f32,
    /// How many lines each message in the open conversation takes, in the order
    /// they are drawn.
    ///
    /// Measured by the renderer — it is the one thing the layout cannot work
    /// out for itself — when the conversation changes rather than every frame.
    /// See `Shell::measure_the_conversation`.
    laid_out: Vec<Laid>,
    /// What the measurement above was made against, so it is made again when
    /// and only when it has to be.
    measured: Option<Measured>,
    /// How tall one line of a message is, and how much air a bubble carries
    /// around its words, in the display's own pixels.
    ///
    /// Told by the layout with the measurement, because the scroll's arithmetic
    /// is done in these and both scale with the screen. Zero until a
    /// conversation has been measured, which is a column of no height and
    /// scrolls nowhere — the honest answer before anything has been drawn.
    line_height: f32,
    message_padding: f32,
    /// How tall the whole column is, and how much of it the body shows.
    chat_column: f32,
    chat_body: f32,
    /// How many lines the draft wraps to, and what that was measured against.
    ///
    /// Measured on its own clock rather than with the column, because it moves
    /// on every keystroke and the column does not.
    draft_lines: u8,
    draft_measured: Option<(String, f32)>,
}

/// One message, and how tall it is.
///
/// Carries which side it belongs on as well as its height, because the layout
/// works from this alone: a pass that had to reach back into the conversation
/// for every rectangle would be two sources for one column, and the two would
/// come apart the moment a message arrived between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Laid {
    pub mark: Mark,
    /// How many lines its body wraps to, at the width the panel gives it.
    pub lines: u8,
    /// And how wide those lines actually come out.
    ///
    /// Measured as well as the line count, because a bubble is a box drawn
    /// *around* words: a message of one short word and one that fills a line
    /// both take one line, and a bubble sized from the count alone is as wide
    /// as the column for both — which takes away the one thing that says which
    /// side of the conversation a message is on without a label.
    pub width: f32,
    /// Whether this account wrote it, which is which side of the column it is
    /// drawn on.
    pub from_me: bool,
    /// Whether it did not go, which is a line of its own under the bubble and
    /// so is room the column has to leave for it.
    pub failed: bool,
}

/// What a measurement of the column was made against.
#[derive(Debug, Clone, PartialEq)]
struct Measured {
    with: u64,
    /// Every mark in the column, in order. The one comparison that catches all
    /// four things that change a message column: one arriving, one leaving, a
    /// pending send being confirmed under Steam's own key, and a whole
    /// conversation being replaced.
    marks: Vec<Mark>,
    /// And the display it was measured for, because the type scales with it.
    height: f32,
}

impl Default for Friends {
    fn default() -> Friends {
        Friends {
            open: false,
            linear: 0.0,
            selected: 0,
            selected_id: None,
            on_status: false,
            scroll: 0.0,
            scroll_speed: 0.0,
            wanted: 0,
            window: 0,
            highlight: crate::menu::Glide::default(),
            talking_to: None,
            turned: 0.0,
            talking: Talking::Compose,
            draft: String::new(),
            composing: false,
            chat_scroll: 0.0,
            chat_speed: 0.0,
            chat_wanted: 0.0,
            laid_out: Vec::new(),
            measured: None,
            line_height: 0.0,
            message_padding: 0.0,
            chat_column: 0.0,
            chat_body: 0.0,
            draft_lines: 1,
            draft_measured: None,
        }
    }
}

impl Friends {
    // -- whether it is there ------------------------------------------------

    /// Whether it is taking input.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether anything of it is on the screen — open, or still sliding out.
    ///
    /// The question every drawing pass asks, and it is deliberately not
    /// [`Self::is_open`]: a panel that stopped being drawn on the frame it was
    /// dismissed would vanish rather than leave. See the standing rule that
    /// nothing disappears before its transition has finished.
    pub fn is_on_screen(&self) -> bool {
        self.open || self.linear > 0.0
    }

    /// Whether it is still moving, so the frame after this one is worth
    /// drawing.
    ///
    /// The slide *or* the scroll: a panel that has settled with a list still
    /// gliding under it is a panel that stops mid-scroll and finishes the next
    /// time something else asks for a frame.
    pub fn is_moving(&self) -> bool {
        (self.linear > 0.0 && self.linear < 1.0) || self.scroll != self.wanted as f32
    }

    /// Raise it. Returns whether it was not already up.
    pub fn open(&mut self) -> bool {
        if self.open {
            return false;
        }
        self.open = true;
        // Not the selection, and not the scroll. Somebody who dismissed the
        // panel to look at something and pressed the button again is coming
        // back to where they were, which is what every column in this shell
        // does. What is reset is the *glide*: the highlight snaps to wherever
        // the row it is on has ended up rather than flying in from where the
        // panel was standing when it was last shut. The scroll snaps for the
        // same reason: a list that flew to where it was left would be a list
        // arriving twice, once with the panel and once on its own.
        self.highlight = crate::menu::Glide::default();
        self.scroll = self.wanted as f32;
        self.scroll_speed = 0.0;
        // And the head's button is given up. It is the one thing here that is
        // *not* remembered between one raising and the next: the panel is
        // raised to see who is online, and it opens on the list every time.
        self.on_status = false;
        true
    }

    /// Dismiss it. Returns whether there was anything to dismiss.
    pub fn close(&mut self) -> bool {
        let was = self.open;
        self.open = false;
        was
    }

    // -- walking it ---------------------------------------------------------

    /// Which friend the highlight is on, given the list as it stands now.
    ///
    /// Clamped rather than trusted, because the list is replaced whenever
    /// anybody in it moves: somebody standing on the last row when a friend
    /// signs out is standing past the end of the list a frame later, and the
    /// honest answer is the last row rather than a panic or an empty
    /// highlight.
    pub fn selected(&self, people: usize) -> usize {
        self.selected.min(people.saturating_sub(1))
    }

    /// Whether the light is on the head's status button.
    ///
    /// `head` is whether there is one to be on — an account is signed in and
    /// Steam is switched on. Masked rather than trusted, for the reason
    /// [`Self::selected`] clamps: both of those can stop being true while the
    /// light is standing on it, and a highlight on a control that is no longer
    /// drawn is a panel with nothing selected and no way to tell.
    pub fn on_status(&self, head: bool) -> bool {
        self.on_status && head
    }

    /// How many lines the panel has room for. Told by the layout.
    pub fn fits(&mut self, lines: usize) {
        self.window = lines;
    }

    /// How far in the panel is, before easing.
    ///
    /// The drawing runs this through [`crate::ui::ease`] once a frame; a hit
    /// test has to run it through the same curve, or a click lands on the row
    /// the panel would have been showing had it travelled linearly.
    pub fn at(&self) -> f32 {
        self.linear
    }

    /// Where the top of the body has got to, in lines. Fractional while the
    /// list is still sliding.
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// And the whole line at or above it, for anything counting rows rather
    /// than drawing them — which is the pass that decides whose picture is
    /// worth fetching.
    pub fn first_line(&self) -> usize {
        self.scroll.max(0.0) as usize
    }

    /// The line the body is heading for. Only the tests and the scroll itself
    /// have any use for this; everything that draws wants [`Self::scroll`].
    #[cfg(test)]
    fn wanted(&self) -> usize {
        self.wanted
    }

    /// Move the highlight up or down. Returns whether it actually moved.
    ///
    /// Does not wrap. A friends list is long — a hundred rows is ordinary — and
    /// a column that jumped from the bottom of it to the top on one press would
    /// be a column nobody can hold a direction on. That is the opposite of the
    /// context menu's rule, and for the opposite reason: that list is four rows
    /// and running off the end of it is more annoying than surprising.
    ///
    /// `head` is whether the head carries a status button — see
    /// [`Self::on_status`]. It sits one line above the first row and is the
    /// only thing above it, so Up off the top of the list is the whole of how
    /// a pad reaches it and Down off it is the way back.
    pub fn move_selection(&mut self, delta: i32, roster: &Roster, head: bool) -> bool {
        // Whatever the light was standing on, against the panel as it is now.
        // The button can go while it is lit — somebody signs out, or Steam is
        // switched off in Settings — and a walk that started from a control
        // nobody can see would be a first press that does nothing.
        self.on_status &= head;
        let people = roster.friends.len();
        if self.on_status {
            // Down off the head and on to the top of the list. Up is nothing:
            // this is the first thing on the panel.
            if delta <= 0 || people == 0 {
                return false;
            }
            self.on_status = false;
            self.selected = 0;
            self.remember_selected(roster);
            self.keep_the_selection_in_view(roster);
            return true;
        }
        if head && delta < 0 && (people == 0 || self.selected(people) == 0) {
            self.on_status = true;
            return true;
        }
        if people == 0 {
            return false;
        }
        let current = self.selected(people);
        let next = (current as i32 + delta).clamp(0, people as i32 - 1) as usize;
        if next == current {
            return false;
        }
        self.selected = next;
        self.remember_selected(roster);
        self.keep_the_selection_in_view(roster);
        true
    }

    /// Put the highlight on the head's status button, where a pointer is
    /// resting over it.
    pub fn point_at_status(&mut self) -> bool {
        if self.on_status {
            return false;
        }
        self.on_status = true;
        true
    }

    /// Put the highlight on the row a pointer is resting over.
    ///
    /// Deliberately does **not** scroll. The hand is on a row it can see, and a
    /// list that shuffled itself to bring that row further into view would move
    /// the very thing being pointed at out from under the pointer — which on a
    /// column this long is a cursor that walks the list on its own. The window
    /// is put right the next time a direction moves the selection, which is
    /// where [`Self::keep_the_selection_in_view`] belongs.
    pub fn point_at(&mut self, person: usize, roster: &Roster) -> bool {
        let people = roster.friends.len();
        if person >= people {
            return false;
        }
        let was = (self.on_status, self.selected(people));
        self.on_status = false;
        self.selected = person;
        self.remember_selected(roster);
        was != (false, person)
    }

    /// Carry the list to `fraction` of the way down itself: a hand on the bar
    /// beside it.
    ///
    /// The window is set from the hand rather than worked back from a row, so
    /// the thumb is exactly where the pointer put it. Then the *selection* is
    /// brought into what is now showing, which is what keeps a mouse and a pad
    /// telling the same story: this shell has one selection, the panel scrolls
    /// to keep it in view, and a view dragged away from it would be pulled
    /// straight back the moment Steam next said anybody had moved — see
    /// [`Self::settle`], which runs on every roster.
    ///
    /// The scroll is snapped rather than sprung. A spring is for a list moving
    /// *itself* under a selection that has walked off the edge; here the hand
    /// is the thing moving, and easing towards where it already is reads as a
    /// bar that does not quite follow.
    pub fn drag_to(&mut self, fraction: f32, roster: &Roster) -> bool {
        let rows = self.window;
        let lines = lines(roster);
        if rows == 0 || lines.len() <= rows {
            return false;
        }
        let most = lines.len() - rows;
        let top = (fraction.clamp(0.0, 1.0) * most as f32).round() as usize;
        let was = (
            self.wanted,
            self.selected(roster.friends.len()),
            self.on_status,
        );
        // A hand on the bar is a hand on the *list*, so the light comes off the
        // head's button and back on to a row — the same thing a hand on a row
        // does. Leaving it up there would be a bar that scrolls a list nothing
        // is standing in.
        self.on_status = false;
        self.wanted = top;
        self.scroll = top as f32;
        self.scroll_speed = 0.0;
        // The selection, where the drag has left it off the screen. The first
        // person inside the window going down, and the last one above it going
        // up, so the highlight arrives at the edge the list came from rather
        // than jumping to the middle of it.
        let selected = self.selected(roster.friends.len());
        let at = lines
            .iter()
            .position(|line| line.person() == Some(selected));
        let showing = top..(top + rows).min(lines.len());
        if !at.is_some_and(|at| showing.contains(&at)) {
            let inside = lines[showing.clone()].iter().find_map(|line| line.person());
            // A window that landed wholly on rules has no row to stand on. It
            // takes a list of one band's heading and nothing else to arrange,
            // and the honest answer is the nearest row below it.
            let next = || lines[top..].iter().find_map(|line| line.person());
            let above = || lines[..top].iter().rev().find_map(|line| line.person());
            if let Some(person) = inside.or_else(next).or_else(above) {
                self.selected = person;
            }
        }
        self.remember_selected(roster);
        was != (
            self.wanted,
            self.selected(roster.friends.len()),
            self.on_status,
        )
    }

    /// Settle where the list is standing against the roster as it now is.
    ///
    /// Called when the panel is raised and whenever Steam sends a new roster,
    /// because both can move the ground under a scroll that was correct when it
    /// was set: a list that is four rows shorter than it was leaves the window
    /// parked past its own end, and the body draws nothing at all. Not called
    /// on every frame — the answer only changes when one of those two things
    /// happens.
    pub fn settle(&mut self, roster: &Roster) {
        let remembered = self.selected_id.and_then(|steam_id| {
            roster
                .friends
                .iter()
                .position(|person| person.steam_id == steam_id)
        });
        if let Some(index) = remembered {
            self.selected = index;
        } else {
            self.selected = self.selected(roster.friends.len());
            self.remember_selected(roster);
        }
        self.keep_the_selection_in_view(roster);
    }

    fn remember_selected(&mut self, roster: &Roster) {
        self.selected_id = roster
            .friends
            .get(self.selected(roster.friends.len()))
            .map(|person| person.steam_id);
    }

    /// Scroll so the selected row is on the screen, and so is the rule above
    /// its band when it is the first row under one.
    ///
    /// The second half is what makes a list of bands readable while it is
    /// walked: stepping on to the first person in Offline with the rule left
    /// one line above the top edge is a row that has stopped saying which band
    /// it is in.
    fn keep_the_selection_in_view(&mut self, roster: &Roster) {
        let rows = self.window;
        if rows == 0 {
            return;
        }
        let lines = lines(roster);
        let selected = self.selected(roster.friends.len());
        let Some(at) = lines
            .iter()
            .position(|line| line.person() == Some(selected))
        else {
            return;
        };
        // The rule above, when the selected row is the first of its band.
        let top = match at.checked_sub(1) {
            Some(above) if matches!(lines.get(above), Some(Line::Heading(..))) => above,
            _ => at,
        };
        self.wanted = self
            .wanted
            .min(top)
            .max((at + 1).saturating_sub(rows))
            .min(lines.len().saturating_sub(rows));
    }

    /// Put the highlight back on the top of the list.
    ///
    /// For the one case a remembered position is the wrong answer: a different
    /// account signed in, which is a different list of people.
    pub fn start_again(&mut self) {
        self.selected = 0;
        self.selected_id = None;
        self.on_status = false;
        self.wanted = 0;
        self.scroll = 0.0;
        self.scroll_speed = 0.0;
        self.highlight = crate::menu::Glide::default();
    }

    // -- moving -------------------------------------------------------------

    /// One frame of the slide, and one of the list's own scroll. Answers how
    /// far in the panel is, before easing.
    pub fn animate(&mut self, dt: f32) -> f32 {
        let target = if self.open { 1.0 } else { 0.0 };
        let step = dt / SLIDE;
        self.linear = if self.linear < target {
            (self.linear + step).min(target)
        } else {
            (self.linear - step).max(target)
        };
        self.scroll_towards_where_it_is_going(dt);
        self.linear
    }

    /// One frame of the list sliding under the selection.
    ///
    /// The same critically damped spring the highlight rides, at the same rate,
    /// and that is the point rather than a coincidence: when the selection
    /// reaches the foot of the window the row it lands on stays where it is on
    /// the screen and the list moves instead, so the two have to move as one
    /// thing. A slower scroll would drag the highlight off its row and a faster
    /// one would leave it behind.
    fn scroll_towards_where_it_is_going(&mut self, dt: f32) {
        let (at, speed) = lxb_protocol::overview::spring(
            self.scroll as f64,
            self.scroll_speed as f64,
            self.wanted as f64,
            crate::menu::HIGHLIGHT_EASE_RATE as f64,
            dt as f64,
        );
        self.scroll = at as f32;
        self.scroll_speed = speed as f32;
        // Settle it outright rather than leaving a hundredth of a line of
        // motion on the books: `is_moving` is what asks for the next frame, and
        // a spring that never quite arrives asks for one for ever.
        if (self.scroll - self.wanted as f32).abs() < SETTLED && self.scroll_speed.abs() < SETTLED {
            self.scroll = self.wanted as f32;
            self.scroll_speed = 0.0;
        }
    }

    /// One frame of the highlight's glide towards `target`, on the same spring
    /// the guide's and the context menu's ride.
    pub fn animate_highlight(&mut self, target: [f32; 4], dt: f32) -> [f32; 4] {
        self.highlight.towards(target, dt)
    }

    // -- the conversation ---------------------------------------------------

    /// Whose conversation is open, by SteamID.
    pub fn talking_to(&self) -> Option<u64> {
        self.talking_to
    }

    /// How far the panel has turned towards the conversation: 0 the list, 1 the
    /// conversation.
    pub fn turned(&self) -> f32 {
        self.turned
    }

    /// Where the light is in the conversation.
    pub fn talking(&self) -> Talking {
        self.talking
    }

    /// What has been typed and not sent.
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// Whether the compose field is being typed into.
    pub fn composing(&self) -> bool {
        self.composing
    }

    /// How far down the message column the view has got, in pixels.
    pub fn chat_scroll(&self) -> f32 {
        self.chat_scroll
    }

    /// How tall each message in the open conversation is.
    pub fn laid_out(&self) -> &[Laid] {
        &self.laid_out
    }

    /// Open somebody's conversation.
    ///
    /// The draft and the scroll are given up, because they belonged to whoever
    /// was open before. Opening the conversation that is *already* open is not
    /// a change and keeps both — which is what makes a second press on the same
    /// row harmless.
    pub fn talk_to(&mut self, steam_id: u64) -> bool {
        if self.talking_to == Some(steam_id) {
            return false;
        }
        self.talking_to = Some(steam_id);
        self.talking = Talking::Compose;
        self.draft.clear();
        self.composing = false;
        // At the foot of the column, which is where a conversation is read
        // from. Snapped rather than sprung: it is arriving, not moving.
        self.chat_scroll = f32::MAX;
        self.chat_wanted = f32::MAX;
        self.chat_speed = 0.0;
        self.laid_out.clear();
        self.measured = None;
        true
    }

    /// Leave the conversation and go back to the list.
    ///
    /// Answers whether there was one to leave. The turn back is what draws it
    /// going; nothing is taken off the screen here.
    pub fn stop_talking(&mut self) -> bool {
        if self.talking_to.take().is_none() {
            return false;
        }
        self.draft.clear();
        self.composing = false;
        true
    }

    /// Begin typing into the compose field, or stop.
    pub fn compose(&mut self, composing: bool) -> bool {
        if self.composing == composing {
            return false;
        }
        self.composing = composing;
        if composing {
            self.talking = Talking::Compose;
        }
        true
    }

    /// Put one keystroke into the draft. Answers whether it changed.
    pub fn type_into_draft(&mut self, character: char) -> bool {
        // Steam's own limit, enforced as it is typed rather than at the send:
        // a field that took four thousand and one characters and quietly sent
        // four thousand would be a message with its last word missing and
        // nothing on the screen to say so.
        if self.draft.chars().count() >= lxb_steam::chat::LONGEST_MESSAGE {
            return false;
        }
        self.draft.push(character);
        true
    }

    /// Take the last character back out of it.
    pub fn rub_out(&mut self) -> bool {
        self.draft.pop().is_some()
    }

    /// Take the draft to send it, leaving the field empty.
    pub fn take_the_draft(&mut self) -> String {
        std::mem::take(&mut self.draft)
    }

    /// Put a draft back, for a send the panel would not take.
    pub fn put_the_draft_back(&mut self, draft: String) {
        self.draft = draft;
    }

    /// Where the light may stand in the conversation as it is now, top to
    /// bottom.
    ///
    /// Built rather than stored, for the reason the roster's [`lines`] is: the
    /// conversation is replaced under the reader and a list held here would be
    /// a second copy of it going stale.
    fn talking_stops(&self, conversation: Option<&Conversation>) -> Vec<Talking> {
        let mut stops = Vec::with_capacity(self.laid_out.len() + 2);
        if conversation.is_some_and(|it| it.history().failure().is_some()) {
            stops.push(Talking::Again);
        }
        stops.extend(self.laid_out.iter().map(|laid| Talking::Message(laid.mark)));
        stops.push(Talking::Compose);
        stops
    }

    /// Move the light up or down inside the conversation.
    ///
    /// Does not wrap, on the same terms the list does not: a conversation is
    /// long, and jumping from the compose field to the oldest message on one
    /// press is a column nobody can hold a direction on.
    pub fn move_in_conversation(
        &mut self,
        delta: i32,
        conversation: Option<&Conversation>,
    ) -> bool {
        let stops = self.talking_stops(conversation);
        if stops.is_empty() {
            return false;
        }
        // Whatever the light was on, against the conversation as it is now. A
        // message can go — a failed send given up on — while the light is
        // standing on it, and the honest answer is the field, which is the one
        // thing a conversation always has. Normalised here as well as in
        // [`Self::measured`], on the terms the roster's `on_status &= head` is:
        // the walk must not start from a place that is not there.
        let mut moved = false;
        let at = match stops.iter().position(|stop| *stop == self.talking) {
            Some(at) => at,
            None => {
                self.talking = stops[stops.len() - 1];
                moved = true;
                stops.len() - 1
            }
        };
        let next = (at as i32 + delta).clamp(0, stops.len() as i32 - 1) as usize;
        if next == at {
            return moved;
        }
        self.talking = stops[next];
        self.keep_the_message_in_view();
        true
    }

    /// Put the light on one message, where a pointer is resting over it.
    pub fn point_at_message(&mut self, mark: Mark) -> bool {
        let was = self.talking;
        self.talking = Talking::Message(mark);
        was != self.talking
    }

    /// And on the compose field.
    pub fn point_at_compose(&mut self) -> bool {
        let was = self.talking;
        self.talking = Talking::Compose;
        was != self.talking
    }

    /// And on the "Try again" a failed history offers.
    pub fn point_at_again(&mut self) -> bool {
        let was = self.talking;
        self.talking = Talking::Again;
        was != self.talking
    }

    /// Say how tall the message column and the body drawn of it are, so the
    /// scroll knows what it is moving over.
    ///
    /// Told by the layout, which is the only thing that knows: heights are
    /// arithmetic on measured line counts and belong in `ui`.
    pub fn the_column_is(&mut self, column: f32, body: f32) {
        self.chat_column = column;
        self.chat_body = body;
        // A view parked past the end of a column that has shrunk — a
        // conversation replaced by a shorter one — draws nothing at all.
        let most = (column - body).max(0.0);
        self.chat_wanted = self.chat_wanted.min(most).max(0.0);
        if self.chat_scroll > most {
            self.chat_scroll = most;
            self.chat_speed = 0.0;
        }
    }

    /// Where the column may be scrolled to at the furthest.
    fn furthest_down(&self) -> f32 {
        (self.chat_column - self.chat_body).max(0.0)
    }

    /// Carry it to `fraction` of the way down itself: a hand on the bar beside
    /// it, snapped rather than sprung for the reason [`Self::drag_to`] is.
    pub fn drag_the_conversation(&mut self, fraction: f32) -> bool {
        let was = self.chat_wanted;
        self.chat_wanted = fraction.clamp(0.0, 1.0) * self.furthest_down();
        self.chat_scroll = self.chat_wanted;
        self.chat_speed = 0.0;
        was != self.chat_wanted
    }

    /// Take the view to the foot of the column, which is where a message
    /// arriving puts it.
    pub fn show_the_latest(&mut self) {
        self.chat_wanted = self.furthest_down();
    }

    /// Whether the view is at the foot of the column, near enough.
    ///
    /// What decides whether a message arriving scrolls the panel. Somebody
    /// reading back through a conversation must not be dragged to the bottom by
    /// a message arriving; somebody at the bottom expects to see it.
    pub fn at_the_latest(&self) -> bool {
        self.chat_wanted >= self.furthest_down() - 1.0
    }

    /// Bring the message the light is on into view.
    fn keep_the_message_in_view(&mut self) {
        let Talking::Message(mark) = self.talking else {
            // The field and the retry are drawn outside the column and are
            // always on screen; standing on the field is standing at the end of
            // the conversation, which is where its scroll belongs.
            if matches!(self.talking, Talking::Compose) {
                self.show_the_latest();
            } else {
                self.chat_wanted = 0.0;
            }
            return;
        };
        let Some((top, height)) = self.where_the_message_is(mark) else {
            return;
        };
        self.chat_wanted = self
            .chat_wanted
            .min(top)
            .max(top + height - self.chat_body)
            .clamp(0.0, self.furthest_down());
    }

    /// Where one message sits inside the column, in pixels from its top.
    fn where_the_message_is(&self, mark: Mark) -> Option<(f32, f32)> {
        let mut y = 0.0;
        for laid in &self.laid_out {
            let height = self.message_height(laid);
            if laid.mark == mark {
                return Some((y, height));
            }
            y += height;
        }
        None
    }

    /// How tall one message is drawn, in the pixels the layout was measured in.
    fn message_height(&self, laid: &Laid) -> f32 {
        self.line_height * laid.lines as f32 + self.message_padding
    }

    /// How many lines the draft wraps to, which is what the field is as tall as.
    pub fn compose_lines(&self) -> u8 {
        self.draft_lines
    }

    /// Whether the field has to be measured again.
    pub fn wants_the_draft_measured(&self, height: f32) -> bool {
        !matches!(&self.draft_measured, Some((draft, at)) if draft == &self.draft && *at == height)
    }

    /// Take that measurement.
    pub fn the_draft_takes(&mut self, lines: u8, height: f32) {
        self.draft_lines = lines.max(1);
        self.draft_measured = Some((self.draft.clone(), height));
    }

    /// Take the measurement the renderer made, if it is one this conversation
    /// still wants.
    ///
    /// `line_height` and `padding` come with it because they are what the
    /// scroll does its arithmetic in, and they scale with the display.
    pub fn measured(
        &mut self,
        with: u64,
        marks: Vec<Mark>,
        laid_out: Vec<Laid>,
        line_height: f32,
        padding: f32,
        height: f32,
    ) {
        self.measured = Some(Measured {
            with,
            marks,
            height,
        });
        self.laid_out = laid_out;
        self.line_height = line_height;
        self.message_padding = padding;
        // And the light, where it was standing on a message this column no
        // longer has: a failed send given up on, or a whole conversation
        // replaced. The field, which is where a conversation always has
        // something to stand on — and where the drawing can find it, so the
        // panel never has a light nothing on it is wearing.
        if let Talking::Message(mark) = self.talking {
            if !self.laid_out.iter().any(|laid| laid.mark == mark) {
                self.talking = Talking::Compose;
            }
        }
    }

    /// Whether the column has to be measured again for this conversation.
    ///
    /// True whenever a mark in it moved, one arrived or left, or the display
    /// changed size — and false on every other frame, which is nearly all of
    /// them. See [`Measured`].
    pub fn wants_measuring(&self, with: u64, marks: &[Mark], height: f32) -> bool {
        !matches!(
            &self.measured,
            Some(measured)
                if measured.with == with && measured.height == height && measured.marks == marks
        )
    }

    /// The marks of an open conversation, in order — what a measurement is
    /// made of and checked against.
    pub fn marks_of(conversation: Option<&Conversation>) -> Vec<Mark> {
        conversation
            .map(|it| it.lines().iter().map(|line| line.mark()).collect())
            .unwrap_or_default()
    }

    /// One frame of the turn between the list and the conversation, and of the
    /// message column's own scroll.
    pub fn animate_conversation(&mut self, dt: f32) -> f32 {
        let target = if self.talking_to.is_some() { 1.0 } else { 0.0 };
        let step = dt / TURN;
        self.turned = if self.turned < target {
            (self.turned + step).min(target)
        } else {
            (self.turned - step).max(target)
        };
        let (at, speed) = lxb_protocol::overview::spring(
            self.chat_scroll as f64,
            self.chat_speed as f64,
            self.chat_wanted as f64,
            crate::menu::HIGHLIGHT_EASE_RATE as f64,
            dt as f64,
        );
        self.chat_scroll = at as f32;
        self.chat_speed = speed as f32;
        if (self.chat_scroll - self.chat_wanted).abs() < SETTLED_PIXELS
            && self.chat_speed.abs() < SETTLED_PIXELS
        {
            self.chat_scroll = self.chat_wanted;
            self.chat_speed = 0.0;
        }
        self.turned
    }

    /// Whether the conversation half is still moving, so the next frame is
    /// worth drawing.
    pub fn conversation_is_moving(&self) -> bool {
        let turning = self.turned > 0.0 && self.turned < 1.0;
        turning || self.chat_scroll != self.chat_wanted
    }

    /// Everything about a conversation given up, because the account has
    /// changed or Steam has been switched off.
    pub fn nothing_to_say(&mut self) -> bool {
        let had = self.talking_to.is_some() || !self.draft.is_empty();
        self.talking_to = None;
        self.talking = Talking::Compose;
        self.draft.clear();
        self.composing = false;
        self.laid_out.clear();
        self.measured = None;
        self.chat_scroll = 0.0;
        self.chat_speed = 0.0;
        self.chat_wanted = 0.0;
        had
    }
}

/// A hundredth of a pixel, which is where the message column's spring is
/// called arrived. See [`SETTLED`], which is the same rule in lines.
const SETTLED_PIXELS: f32 = 0.01;

#[cfg(test)]
mod tests {
    use super::*;
    use lxb_steam::{Person, Presence};

    fn person(name: &str, presence: Presence, game: Option<&str>) -> Person {
        Person {
            steam_id: name.len() as u64,
            name: name.to_string(),
            presence,
            game: game.map(str::to_string),
            app_id: game.map(|_| 220),
            avatar: None,
        }
    }

    fn roster(friends: Vec<Person>) -> Roster {
        // Sorted the way Steam's half of this hands it over, so a test builds
        // the same list the panel is ever given.
        let mut friends = friends;
        friends.sort_by_key(|friend| friend.band());
        Roster { me: None, friends }
    }

    // --- the conversation --------------------------------------------------

    fn with_ids(names: &[(&str, Presence, u64)]) -> Roster {
        let mut friends: Vec<Person> = names
            .iter()
            .map(|(name, presence, steam_id)| Person {
                steam_id: *steam_id,
                name: name.to_string(),
                presence: *presence,
                game: None,
                app_id: None,
                avatar: None,
            })
            .collect();
        friends.sort_by_key(|friend| friend.band());
        Roster { me: None, friends }
    }

    fn laid(marks: &[(Mark, bool)]) -> Vec<Laid> {
        marks
            .iter()
            .map(|(mark, from_me)| Laid {
                mark: *mark,
                lines: 1,
                width: 40.0,
                from_me: *from_me,
                failed: false,
            })
            .collect()
    }

    fn said(at: u32) -> Mark {
        Mark::Said(lxb_steam::chat::Key::new(at, 0))
    }

    /// The whole of the SteamID rule: a friend who moves band under the reader
    /// keeps the highlight, and so does an open conversation.
    #[test]
    fn presence_reordering_the_list_does_not_move_the_selection() {
        let mut friends = Friends::default();
        friends.fits(10);
        let before = with_ids(&[
            ("Ann", Presence::Online, 11),
            ("Bea", Presence::Online, 22),
            ("Cal", Presence::Online, 33),
        ]);
        friends.settle(&before);
        assert!(friends.move_selection(1, &before, false));
        assert_eq!(before.friends[friends.selected(3)].steam_id, 22);

        // Ann starts a game, which puts her in a band above everybody.
        let after = with_ids(&[
            ("Bea", Presence::Online, 22),
            ("Cal", Presence::Online, 33),
            ("Ann", Presence::Offline, 11),
        ]);
        friends.settle(&after);
        assert_eq!(
            after.friends[friends.selected(3)].steam_id,
            22,
            "the highlight followed the row number rather than the person"
        );
    }

    /// A conversation is held by SteamID, so nothing the roster does moves it.
    #[test]
    fn a_conversation_is_held_by_steam_id() {
        let mut friends = Friends::default();
        assert!(friends.talk_to(22));
        assert_eq!(friends.talking_to(), Some(22));
        // The list is replaced under it — everybody has moved band.
        friends.settle(&with_ids(&[
            ("Cal", Presence::Offline, 33),
            ("Bea", Presence::Away, 22),
        ]));
        assert_eq!(friends.talking_to(), Some(22));
    }

    /// Opening the same conversation twice keeps the draft; opening a different
    /// one gives it up, because it was written to somebody else.
    #[test]
    fn switching_conversations_gives_up_the_draft() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        friends.compose(true);
        for character in "half a sentence".chars() {
            friends.type_into_draft(character);
        }
        assert!(
            !friends.talk_to(22),
            "opening the same one again is no change"
        );
        assert_eq!(friends.draft(), "half a sentence");
        assert!(friends.talk_to(33));
        assert_eq!(friends.draft(), "");
        assert!(
            !friends.composing(),
            "the field was left up over somebody else"
        );
    }

    /// Rapid switching, which is the same rule stated as a sequence: whatever
    /// order the presses come in, the panel is talking to the last one pressed
    /// and holding nothing of the others.
    #[test]
    fn rapid_switching_lands_on_the_last_one_pressed() {
        let mut friends = Friends::default();
        for id in [11, 22, 33, 22, 44] {
            friends.talk_to(id);
            friends.compose(true);
            friends.type_into_draft('x');
        }
        assert_eq!(friends.talking_to(), Some(44));
        assert_eq!(friends.draft(), "x");
    }

    /// The walk inside a conversation is by mark, so a message arriving above
    /// the light does not move it.
    #[test]
    fn the_light_in_a_conversation_is_held_by_mark() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        friends.measured(
            22,
            vec![said(100), said(200)],
            laid(&[(said(100), false), (said(200), true)]),
            10.0,
            4.0,
            1080.0,
        );
        // Up from the field on to the last message, then up again.
        assert!(friends.move_in_conversation(-1, None));
        assert_eq!(friends.talking(), Talking::Message(said(200)));
        assert!(friends.move_in_conversation(-1, None));
        assert_eq!(friends.talking(), Talking::Message(said(100)));
        // An older message arrives at the head of the column.
        friends.measured(
            22,
            vec![said(50), said(100), said(200)],
            laid(&[(said(50), false), (said(100), false), (said(200), true)]),
            10.0,
            4.0,
            1080.0,
        );
        assert_eq!(
            friends.talking(),
            Talking::Message(said(100)),
            "a message arriving above the light moved it"
        );
        // And down is still the next one along rather than back to the start.
        assert!(friends.move_in_conversation(1, None));
        assert_eq!(friends.talking(), Talking::Message(said(200)));
    }

    /// A message the light is standing on can go — a failed send given up on —
    /// and the honest answer is the field, which a conversation always has.
    #[test]
    fn losing_the_message_under_the_light_falls_back_to_the_field() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        friends.measured(
            22,
            vec![Mark::Pending(4)],
            laid(&[(Mark::Pending(4), true)]),
            10.0,
            4.0,
            1080.0,
        );
        assert!(friends.move_in_conversation(-1, None));
        assert_eq!(friends.talking(), Talking::Message(Mark::Pending(4)));
        friends.measured(22, Vec::new(), Vec::new(), 10.0, 4.0, 1080.0);
        // Nothing is left but the field, and a press has somewhere to land.
        assert!(!friends.move_in_conversation(1, None));
        assert_eq!(friends.talking(), Talking::Compose);
    }

    /// Steam's limit is enforced as the message is typed, not at the send.
    #[test]
    fn the_field_stops_at_steam_s_limit() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        for _ in 0..lxb_steam::chat::LONGEST_MESSAGE {
            assert!(friends.type_into_draft('é'));
        }
        assert!(
            !friends.type_into_draft('é'),
            "the field took more than Steam will"
        );
        assert_eq!(
            friends.draft().chars().count(),
            lxb_steam::chat::LONGEST_MESSAGE
        );
        assert!(friends.rub_out());
        assert!(friends.type_into_draft('é'));
    }

    /// The column is measured again when — and only when — something about it
    /// changed.
    #[test]
    fn the_column_is_measured_only_when_it_moves() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        friends.measured(
            22,
            vec![said(100)],
            laid(&[(said(100), false)]),
            10.0,
            4.0,
            1080.0,
        );
        // Nothing has moved.
        assert!(!friends.wants_measuring(22, &[said(100)], 1080.0));
        // A message arrives.
        assert!(friends.wants_measuring(22, &[said(100), said(200)], 1080.0));
        // A pending send is confirmed under Steam's own key, which changes a
        // mark without changing how many there are — the case a length check
        // would miss.
        assert!(friends.wants_measuring(22, &[Mark::Pending(1)], 1080.0));
        // The display changed size, so the type did too.
        assert!(friends.wants_measuring(22, &[said(100)], 720.0));
        // A different conversation entirely.
        assert!(friends.wants_measuring(33, &[said(100)], 1080.0));
    }

    /// Leaving a conversation gives up the draft and the field; leaving the
    /// panel keeps the conversation, because it is come back to.
    #[test]
    fn what_each_way_out_gives_up() {
        let mut friends = Friends::default();
        friends.open();
        friends.talk_to(22);
        friends.compose(true);
        friends.type_into_draft('x');
        // Out of the field: the draft stays, because nothing was sent.
        assert!(friends.compose(false));
        assert_eq!(friends.draft(), "x");
        assert_eq!(friends.talking_to(), Some(22));
        // Out of the conversation: the draft goes with it.
        assert!(friends.stop_talking());
        assert_eq!(friends.draft(), "");
        assert!(friends.talking_to().is_none());
        // Out of the panel, with a conversation open: it is still open when it
        // comes back, which is what every column in this shell does.
        friends.talk_to(22);
        assert!(friends.close());
        assert!(friends.open());
        assert_eq!(friends.talking_to(), Some(22));
    }

    /// The message column scrolls in pixels, opens at its foot, and is carried
    /// to the end by a message arriving only when the reader is already there.
    #[test]
    fn the_message_column_scrolls_and_stays_where_it_is_put() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        // Nothing measured yet: a column of no height scrolls nowhere, and the
        // view asked for the end of it settles at the top rather than at
        // whatever [`Self::talk_to`] put there.
        friends.the_column_is(0.0, 400.0);
        assert_eq!(friends.chat_scroll(), 0.0);
        assert!(friends.at_the_latest());

        // A column three bodies tall, opened at its foot.
        friends.talk_to(33);
        friends.the_column_is(1200.0, 400.0);
        assert!(friends.at_the_latest());
        assert_eq!(friends.chat_scroll(), 800.0, "it did not open at the end");

        // Dragged to the middle: snapped rather than sprung, and no longer at
        // the end — so a message arriving must not carry the reader with it.
        assert!(friends.drag_the_conversation(0.5));
        assert_eq!(friends.chat_scroll(), 400.0);
        assert!(!friends.at_the_latest());

        // Past either end is clamped rather than allowed.
        friends.drag_the_conversation(-1.0);
        assert_eq!(friends.chat_scroll(), 0.0);
        friends.drag_the_conversation(2.0);
        assert_eq!(friends.chat_scroll(), 800.0);

        // And a column that shrinks under a view parked past its own end — a
        // conversation replaced by a shorter one — brings the view back rather
        // than drawing nothing at all.
        friends.the_column_is(500.0, 400.0);
        assert_eq!(friends.chat_scroll(), 100.0);
    }

    /// A different account takes every conversation with it.
    #[test]
    fn a_new_account_leaves_nothing_to_say() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        friends.compose(true);
        friends.type_into_draft('x');
        assert!(friends.nothing_to_say());
        assert!(friends.talking_to().is_none());
        assert_eq!(friends.draft(), "");
        assert!(!friends.composing());
        assert_eq!(friends.talking(), Talking::Compose);
        assert!(
            !friends.nothing_to_say(),
            "there was nothing left to give up"
        );
    }

    /// The failed history's retry stands above the column, and is only there
    /// while there is a failure to retry.
    #[test]
    fn the_retry_is_a_stop_only_while_the_history_failed() {
        let mut friends = Friends::default();
        friends.talk_to(22);
        friends.measured(
            22,
            vec![said(100)],
            laid(&[(said(100), false)]),
            10.0,
            4.0,
            1080.0,
        );
        // Without a failure: the field and one message.
        assert!(friends.move_in_conversation(-1, None));
        assert_eq!(friends.talking(), Talking::Message(said(100)));
        assert!(
            !friends.move_in_conversation(-1, None),
            "there is nothing above the first message"
        );
    }

    #[test]
    fn a_band_with_nobody_in_it_gets_no_rule() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        assert_eq!(
            lines(&roster),
            [
                Line::Heading(Band::Around, 2),
                Line::Person(0),
                Line::Person(1)
            ]
        );
    }

    #[test]
    fn each_band_is_ruled_off_and_counted() {
        let roster = roster(vec![
            person("Ann", Presence::Online, Some("Portal 2")),
            person("Bea", Presence::Online, None),
            person("Cal", Presence::Offline, None),
            person("Dee", Presence::Offline, None),
        ]);
        assert_eq!(
            lines(&roster),
            [
                Line::Heading(Band::Playing, 1),
                Line::Person(0),
                Line::Heading(Band::Around, 1),
                Line::Person(1),
                Line::Heading(Band::Away, 2),
                Line::Person(2),
                Line::Person(3),
            ]
        );
    }

    #[test]
    fn an_empty_roster_is_an_empty_panel() {
        assert!(lines(&Roster::default()).is_empty());
        let mut friends = Friends::default();
        assert!(!friends.move_selection(1, &Roster::default(), false));
        assert_eq!(friends.selected(0), 0);
    }

    /// The list is replaced whenever anybody moves, so a selection past the end
    /// of the new one is the ordinary case rather than a bug to panic on.
    #[test]
    fn a_selection_past_the_end_of_a_shorter_list_is_its_last_row() {
        let friends = Friends {
            selected: 9,
            ..Friends::default()
        };
        assert_eq!(friends.selected(3), 2);
        assert_eq!(friends.selected(0), 0);
    }

    #[test]
    fn the_highlight_does_not_wrap_off_either_end() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        let mut friends = Friends::default();
        assert!(!friends.move_selection(-1, &roster, false));
        assert!(friends.move_selection(1, &roster, false));
        assert_eq!(friends.selected(2), 1);
        assert!(!friends.move_selection(1, &roster, false));
    }

    /// Stepping on to the first row of a band brings its rule with it.
    #[test]
    fn a_bands_rule_comes_into_view_with_its_first_row() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
            person("Cal", Presence::Offline, None),
        ]);
        let mut friends = Friends::default();
        // Room for three of the five lines.
        friends.fits(3);
        assert!(friends.move_selection(1, &roster, false));
        assert_eq!(friends.wanted(), 0);
        // Cal is line 4; its rule is line 3, and both have to be on screen.
        assert!(friends.move_selection(1, &roster, false));
        assert_eq!(friends.wanted(), 2);
    }

    /// The list *slides* to where it is going rather than stepping there.
    ///
    /// The whole of what the user asked for: an integer first-row moves the
    /// column by a row's height on one frame, and no easing anywhere else can
    /// hide that. See [`Friends::scroll`].
    #[test]
    fn the_list_slides_to_where_it_is_going() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
            person("Cal", Presence::Offline, None),
            person("Dee", Presence::Offline, None),
        ]);
        let mut friends = Friends::default();
        friends.fits(3);
        friends.open();
        // Down far enough that the window has to move.
        while friends.move_selection(1, &roster, false) {}
        assert!(friends.wanted() > 0, "the window moved");
        // It has not gone anywhere yet, and it is not there after one frame
        // either — it is on its way, which is the point.
        assert_eq!(friends.scroll(), 0.0);
        assert!(friends.is_moving());
        friends.animate(1.0 / 60.0);
        let first = friends.scroll();
        assert!(first > 0.0 && first < friends.wanted() as f32, "{first}");
        friends.animate(1.0 / 60.0);
        assert!(
            friends.scroll() > first,
            "still going: {}",
            friends.scroll()
        );
        // And it settles outright rather than creeping towards its target for
        // ever, because "still moving" is what asks for the next frame.
        for _ in 0..120 {
            friends.animate(1.0 / 60.0);
        }
        assert_eq!(friends.scroll(), friends.wanted() as f32);
        assert!(!friends.is_moving());
    }

    /// A pointer resting on a row takes it, and moves the list not at all: a
    /// column that shuffled itself to bring the pointed-at row further into
    /// view would move that very row out from under the pointer.
    #[test]
    fn pointing_at_a_row_takes_it_without_moving_the_list() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
            person("Cal", Presence::Offline, None),
            person("Dee", Presence::Offline, None),
        ]);
        let mut friends = Friends::default();
        friends.fits(3);
        friends.open();
        assert!(friends.point_at(3, &roster));
        assert_eq!(friends.selected(roster.friends.len()), 3);
        assert_eq!(friends.wanted(), 0, "the window stayed where it was");
        assert_eq!(friends.scroll(), 0.0);
        // Twice on the same row is once.
        assert!(!friends.point_at(3, &roster));
        // And nobody who is not there.
        assert!(!friends.point_at(9, &roster));
        assert_eq!(friends.selected(roster.friends.len()), 3);
    }

    /// A hand on the bar puts the window where it was put, and brings the
    /// selection into what is now showing — this shell has one selection, and
    /// a view dragged away from it would be pulled back the moment Steam next
    /// said anybody had moved.
    #[test]
    fn a_hand_on_the_bar_carries_the_list_and_the_selection_with_it() {
        let roster = roster(
            (0..20)
                .map(|n| person(&format!("friend {n}"), Presence::Online, None))
                .collect(),
        );
        let mut friends = Friends::default();
        friends.fits(6);
        friends.open();
        let lines = lines(&roster);
        let most = lines.len() - 6;

        // All the way down: the last window, and somebody in it.
        assert!(friends.drag_to(1.0, &roster));
        assert_eq!(friends.wanted(), most);
        assert_eq!(friends.scroll(), most as f32, "snapped, not sprung");
        assert!(!friends.is_moving());
        let at = lines
            .iter()
            .position(|line| line.person() == Some(friends.selected(roster.friends.len())))
            .expect("a row");
        assert!(
            at >= most && at < most + 6,
            "the selection is off the panel"
        );

        // And back to the top.
        assert!(friends.drag_to(0.0, &roster));
        assert_eq!(friends.wanted(), 0);
        assert_eq!(friends.selected(roster.friends.len()), 0);

        // Half way is half way down the list rather than half way down the
        // people: the band's rule is a line of the column too.
        assert!(friends.drag_to(0.5, &roster));
        assert_eq!(friends.wanted(), (most as f32 * 0.5).round() as usize);
    }

    /// A list that fits has nothing to drag, and says so rather than jumping.
    #[test]
    fn a_list_that_fits_cannot_be_dragged() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        let mut friends = Friends::default();
        friends.fits(8);
        friends.open();
        assert!(!friends.drag_to(1.0, &roster));
        assert_eq!(friends.wanted(), 0);
        assert_eq!(friends.selected(roster.friends.len()), 0);
        // And a panel nobody has told how big the screen is does not guess.
        let mut untold = Friends::default();
        assert!(!untold.drag_to(1.0, &roster));
    }

    /// Coming back to the panel is coming back to the list where it was, not
    /// watching it fly there.
    #[test]
    fn a_reopened_panel_does_not_scroll_into_place() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
            person("Cal", Presence::Offline, None),
            person("Dee", Presence::Offline, None),
        ]);
        let mut friends = Friends::default();
        friends.fits(3);
        while friends.move_selection(1, &roster, false) {}
        friends.close();
        friends.open();
        assert_eq!(friends.scroll(), friends.wanted() as f32);
        assert!(!friends.is_moving());
    }

    /// The panel leaves rather than vanishing, and comes back where it was.
    #[test]
    fn it_slides_both_ways_and_is_drawn_the_whole_time() {
        let mut friends = Friends::default();
        assert!(!friends.is_on_screen());
        assert!(friends.open());
        for _ in 0..8 {
            friends.animate(SLIDE / 4.0);
        }
        assert_eq!(friends.animate(0.0), 1.0);
        assert!(friends.close());
        // Dismissed, and still on screen: it is on its way out.
        assert!(!friends.is_open());
        friends.animate(SLIDE / 2.0);
        assert!(friends.is_on_screen());
        assert!(friends.is_moving());
        friends.animate(SLIDE);
        assert!(!friends.is_on_screen());
    }

    /// A panel dismissed half way in goes no further first.
    #[test]
    fn a_panel_turned_back_half_way_does_not_finish_arriving() {
        let mut friends = Friends::default();
        friends.open();
        friends.animate(SLIDE / 2.0);
        let half = friends.animate(0.0);
        assert!((half - 0.5).abs() < 0.01);
        friends.close();
        assert!(friends.animate(SLIDE / 4.0) < half);
    }

    /// Coming back to the panel is coming back to where you were standing.
    #[test]
    fn it_reopens_where_it_was_left() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        let mut friends = Friends::default();
        friends.move_selection(1, &roster, false);
        friends.close();
        friends.open();
        assert_eq!(friends.selected(2), 1);
    }

    /// A list that has grown shorter since the window was set does not leave
    /// the body parked past its own end, drawing nothing.
    #[test]
    fn a_shorter_list_brings_the_window_back() {
        let long = roster(
            (0..12)
                .map(|index| person(&format!("friend {index}"), Presence::Online, None))
                .collect(),
        );
        let mut friends = Friends::default();
        friends.fits(4);
        while friends.move_selection(1, &long, false) {}
        assert!(friends.wanted() > 0);
        // Everybody but two signed out while the panel was shut.
        let short = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        friends.settle(&short);
        assert_eq!(friends.wanted(), 0, "the window is back over the rows");
        assert_eq!(friends.selected(short.friends.len()), 1);
    }

    #[test]
    fn a_reordered_roster_keeps_the_same_person_selected() {
        let mut ann = person("Ann", Presence::Online, None);
        ann.steam_id = 1;
        let mut bea = person("Bea", Presence::Online, None);
        bea.steam_id = 2;
        let mut friends = Friends::default();
        let before = roster(vec![ann.clone(), bea.clone()]);
        friends.settle(&before);
        assert!(friends.move_selection(1, &before, false));
        assert_eq!(before.friends[friends.selected(2)].steam_id, 2);

        // Bea starts a game and moves to the first band, changing her row
        // number without changing who the cursor is standing on.
        bea.game = Some("Portal 2".to_string());
        bea.app_id = Some(620);
        let after = roster(vec![ann, bea]);
        friends.settle(&after);
        assert_eq!(after.friends[friends.selected(2)].steam_id, 2);
    }

    /// Unless somebody else signed in, which is a different list of people.
    #[test]
    fn a_new_account_starts_at_the_top() {
        let mut friends = Friends {
            selected: 4,
            scroll: 2.0,
            wanted: 2,
            ..Friends::default()
        };
        friends.start_again();
        assert_eq!(friends.selected(9), 0);
        assert_eq!(friends.scroll(), 0.0);
    }

    /// The status button is the first thing on the panel and is never where the
    /// panel opens. Somebody pressed a button to see who is online; a column
    /// that greeted them with their own status lit answered a different
    /// question.
    #[test]
    fn the_panel_never_opens_on_the_head() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        let mut friends = Friends::default();
        friends.fits(8);
        friends.open();
        assert!(!friends.on_status(true), "it opened on the head");

        // Left there and raised again, it is still not: this is the one thing
        // the panel does not remember between one raising and the next.
        assert!(friends.move_selection(-1, &roster, true));
        assert!(friends.on_status(true));
        friends.close();
        friends.open();
        assert!(!friends.on_status(true));
        assert_eq!(friends.selected(2), 0);
    }

    /// Up off the top of the list reaches it, and Down comes back to the first
    /// row. That is the whole of how a pad gets there: it is above the rows and
    /// nothing else is.
    #[test]
    fn the_head_is_one_line_above_the_first_row() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        let mut friends = Friends::default();
        friends.fits(8);
        friends.open();

        // Down the list and back up to the top: the head is reached from the
        // first row and from nowhere else.
        assert!(friends.move_selection(1, &roster, true));
        assert!(!friends.on_status(true));
        assert!(friends.move_selection(-1, &roster, true));
        assert!(!friends.on_status(true), "row 1 to row 0, not to the head");
        assert!(friends.move_selection(-1, &roster, true));
        assert!(friends.on_status(true));

        // And Up again is nothing: it is the first thing on the panel.
        assert!(!friends.move_selection(-1, &roster, true));
        assert!(friends.on_status(true));

        assert!(friends.move_selection(1, &roster, true));
        assert!(!friends.on_status(true));
        assert_eq!(friends.selected(2), 0);
    }

    /// A panel with no list still has the head, and a panel with no head still
    /// has the list. Neither direction may walk on to something that is not
    /// drawn.
    #[test]
    fn a_button_that_is_not_there_cannot_be_stood_on() {
        let mut friends = Friends::default();
        friends.fits(8);
        friends.open();

        // Nobody signed in: the button is not drawn, so Up off the top of the
        // list is nothing at all.
        let roster = roster(vec![person("Ann", Presence::Online, None)]);
        assert!(!friends.move_selection(-1, &roster, false));
        assert!(!friends.on_status(true));

        // Signed in and with an empty list, the head is the only thing there
        // is — and it is still reached by pressing Up rather than by being
        // where the panel opened.
        let empty = Roster::default();
        assert!(!friends.on_status(true));
        assert!(friends.move_selection(-1, &empty, true));
        assert!(friends.on_status(true));
        assert!(
            !friends.move_selection(1, &empty, true),
            "no list to step on to"
        );
        assert!(friends.on_status(true));
    }

    /// The button can go while the light is standing on it — somebody signs
    /// out, or Steam is switched off in Settings. The light is on the list
    /// again from that moment, and the first direction pressed walks the list.
    #[test]
    fn a_light_on_a_button_that_has_gone_is_a_light_on_the_list() {
        let roster = roster(vec![
            person("Ann", Presence::Online, None),
            person("Bea", Presence::Online, None),
        ]);
        let mut friends = Friends::default();
        friends.fits(8);
        friends.open();
        assert!(friends.move_selection(-1, &roster, true));
        assert!(friends.on_status(true));
        // Masked the moment it is asked without a button to be on.
        assert!(!friends.on_status(false));
        // And the walk that follows is the list's, from the row the light was
        // last on rather than from the head.
        assert!(friends.move_selection(1, &roster, false));
        assert_eq!(friends.selected(2), 1);
    }

    /// A hand on a row, and a hand on the bar, both bring the light back down
    /// out of the head. The pointer and the pad tell one story.
    #[test]
    fn a_hand_on_the_list_takes_the_light_off_the_head() {
        let roster = roster(
            (0..30)
                .map(|n| person(&format!("friend {n}"), Presence::Online, None))
                .collect(),
        );
        let mut friends = Friends::default();
        friends.fits(6);
        friends.open();

        assert!(friends.move_selection(-1, &roster, true));
        assert!(friends.on_status(true));
        assert!(friends.point_at(4, &roster));
        assert!(!friends.on_status(true));
        assert_eq!(friends.selected(30), 4);

        assert!(friends.move_selection(-1, &roster, true) || friends.on_status(true));
        friends.point_at_status();
        assert!(friends.on_status(true));
        assert!(friends.drag_to(1.0, &roster));
        assert!(!friends.on_status(true), "a hand on the bar is on the list");
    }

    /// Pointing at the head twice is one move, not two: a pointer resting still
    /// must not ask for a frame on every motion event under it.
    #[test]
    fn pointing_at_the_head_twice_says_nothing_the_second_time() {
        let mut friends = Friends::default();
        assert!(friends.point_at_status());
        assert!(!friends.point_at_status());
    }
}
