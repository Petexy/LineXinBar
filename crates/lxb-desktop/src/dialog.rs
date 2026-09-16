//! The centred panel: something the shell has to say about one object, or one
//! question about it, with the answers underneath.
//!
//! [`crate::menu`] is a note pinned to a control — it stands beside the thing
//! it is about, because the thing it is about is the only context it has. This
//! is the other half of that: a panel that has *taken over*, in the middle of
//! the display, carrying enough of its subject inside it that the screen behind
//! it no longer has to be readable. Information about an application is one;
//! "do you want to uninstall this" is another; and so are the two questions
//! that arrive from outside the shell altogether — may this application see the
//! screen, and prove that you may do this (see [`crate::polkit`]).
//!
//! It is a component on the same terms as the menu, and for the same reason —
//! there will be more of these than there are today. Whoever raises one hands
//! over the rectangle it grows out of, what it says, and what can be pressed:
//!
//! ```ignore
//! dialog.ask(
//!     chosen_row_rect,
//!     None,
//!     vec![
//!         Line::Note(format!("Do you want to uninstall {name}?")),
//!         Line::Rule,
//!     ],
//!     vec![
//!         Entry::new(Command::Dismiss, "No"),
//!         Entry::new(Command::ConfirmUninstall, "Yes").grave(),
//!     ],
//!     // Opens on No, which is also the answer drawn first, because this one
//!     // destroys something.
//!     0,
//! );
//! ```
//!
//! What it says is a list of [`Line`]s rather than a set of named fields, so a
//! panel that wants a heading and two values and a panel that wants one
//! sentence are the same type with different contents, and neither has to be
//! taught to the drawing separately.
//!
//! ## Why the buttons are a `Menu`
//!
//! A column of answers *is* a short column of commands: it moves the same way,
//! presses the same way, and its highlight glides on the same spring. Holding a
//! [`Menu`] here rather than writing those three things again is what keeps a
//! Yes button and a menu row from drifting into feeling like controls from two
//! different shells — the same argument the panel's own glass is cut from
//! `sidebar_surface` for.

use crate::menu::{Entry, Menu};

/// One line of what the panel says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// The name of the thing the panel is about, at its head.
    Heading(String),
    /// A sentence: what an application is for, or the question being asked.
    Note(String),
    /// A named value — the label on the left, the answer on the right.
    Field { label: String, value: String },
    /// A field being typed into, drawn as a box with one mark per character
    /// and never the characters themselves.
    ///
    /// A count rather than the text, deliberately: the panel is drawn from
    /// whatever is in this list, so a `Line` that carried the password would be
    /// a copy of it living in the layout for as long as the panel was up. What
    /// is actually typed stays in a [`crate::secret::Secret`], which is the
    /// only thing in the shell built to hold one.
    Secret { typed: usize },
    /// A field being typed into whose contents are *not* a secret — an account
    /// name, a code from an email — drawn as the same well with the characters
    /// in it.
    ///
    /// The text rather than a count, and that is the whole difference from
    /// [`Line::Secret`]: a code that could not be read back is a code nobody
    /// can check they typed correctly, and a person entering an account name
    /// on a television with a thumbstick needs to see it more than anyone.
    Entry(String),
    /// The code the Steam app on a phone photographs, as the squares it is
    /// made of.
    ///
    /// Carried as the grid rather than as a picture: the shell draws
    /// everything as quads, so a module is a quad and nothing is rasterised,
    /// scaled or uploaded for it. How large a module is drawn is the panel's
    /// business — see [`crate::ui`] — because what makes a code readable to a
    /// camera is the module being a whole number of pixels.
    Qr(lxb_steam::qr::Code),
    /// Something is happening elsewhere and there is nothing to show for it
    /// yet: a row of lights, moving.
    ///
    /// A line rather than a spinner drawn over the panel, because it stands in
    /// a place — where the code will be, where the answer will be — and a
    /// panel that grew by the height of a QR code a moment after it opened
    /// would be the shell moving the buttons out from under somebody's thumb.
    Waiting,
    /// Something is happening elsewhere that *can* say how far along it is: a
    /// groove, and the part of it that has been done.
    ///
    /// Exactly as tall as [`Line::Waiting`] and drawn in the same place, which
    /// is the whole reason it is a line of its own rather than a field of it.
    /// The one thing this is used for — Steam installing itself — spends its
    /// first seconds with nothing to count and its last minute unpacking
    /// something whose length nobody knows, so the panel goes from lights to a
    /// bar and back again while it runs, and it must not change size doing it.
    ///
    /// Whole percent, which is Valve's own granularity and a television's. See
    /// [`lxb_steam::setup::Step::percent`].
    Progress(u8),
    /// A hairline, where one band of the panel gives way to the next. Drawn
    /// rather than implied by a gap, because two of these panels are a list of
    /// facts and a list is easier to read against a rule than against air.
    Rule,
    /// What a program said, as the terminal it said it on: a dark well with
    /// the lines in it, left-aligned in the fixed-width face, exactly the
    /// width of the terminal they were written to.
    ///
    /// The one line of the panel that is not the shell's own words. Every
    /// other kind here is a label or a sentence the shell wrote and knows the
    /// length of; this is a transcript somebody else wrote, thousands of lines
    /// of it, laid out in columns that only a fixed-width face keeps. So it
    /// is drawn as its own object — the panel widens for it, see
    /// [`crate::ui::dialog_rect`] — and it is never wrapped, cut or centred:
    /// a line that fit the terminal fits the frame, because the frame is
    /// [`lxb_updates::COLUMNS`] of the same width.
    ///
    /// The caller hands over the *window* — the lines to show, top to bottom
    /// — and says in the foot where in the whole that window is. The frame is
    /// `rows` tall whether or not there are lines to fill it, so a transcript
    /// that has only just started does not draw a panel that grows a line at
    /// a time under the reader.
    Terminal {
        lines: Vec<String>,
        rows: usize,
        /// Where the window is in the whole, and how to move it, along the
        /// frame's foot: "Lines 121–140 of 300 · Left and Right to scroll".
        foot: String,
    },
}

impl Line {
    pub fn field(label: impl Into<String>, value: impl Into<String>) -> Self {
        Line::Field {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// The panel as the shell holds it.
///
/// One per session, like the menu and for the same reason: it takes every
/// button while it is up, so a second would be a second answer to the question
/// of who has the keys. It can nonetheless be *on screen* at the same time as
/// the menu that raised it — that menu is still folding back into its anchor —
/// which is why the two are separate fields of the shell rather than one.
#[derive(Debug, Default)]
pub struct Dialog {
    /// The answers, and every part of how they behave: which one is
    /// highlighted, how the light glides between them, how far the panel is out
    /// of the control it came from, and which button is being watched going
    /// down. See the module docs for why this is a whole [`Menu`].
    pub buttons: Menu,
    icon: Option<String>,
    lines: Vec<Line>,
    /// How much of the menu's own departure is still to be waited out before
    /// this panel starts growing, in seconds. See [`Dialog::animate`].
    wait: f32,
    /// How much of the foot of the display something else has taken, in
    /// pixels. See [`Dialog::set_footer`].
    footer: f32,
}

fn wrap_notes(lines: Vec<Line>) -> Vec<Line> {
    lines
        .into_iter()
        .flat_map(|line| match line {
            Line::Note(text) => crate::gpu::wrap_dialog_note(&text)
                .into_iter()
                .map(Line::Note)
                .collect(),
            other => vec![other],
        })
        .collect()
}

impl Dialog {
    /// Raise the panel out of `anchor` — the control that was pressed to open
    /// it — saying `lines`, offering `buttons`, opening on button `start`.
    ///
    /// Returns whether it opened, which it does not if nothing on it can be
    /// pressed: a modal panel with no way out is the one failure a user cannot
    /// recover from without the guide.
    pub fn ask(
        &mut self,
        anchor: [f32; 4],
        icon: Option<String>,
        lines: Vec<Line>,
        buttons: Vec<Entry>,
        start: usize,
    ) -> bool {
        let count = buttons.len();
        // The answers never scroll. A question with more answers than fit on
        // the display is a question that has been written wrongly, and hiding
        // half of them under an arrow would not be the fix.
        if !self
            .buttons
            .open_selecting(anchor, None, buttons, count, start)
        {
            return false;
        }
        self.icon = icon;
        self.lines = wrap_notes(lines);
        self.wait = crate::menu::PRESS_TIME;
        true
    }

    /// Raise a panel with nothing to press: the shell is busy behind it, and
    /// something else will replace it when that finishes.
    ///
    /// It waits for the menu that raised it exactly as [`Self::ask`] does, so
    /// that the first panel of a sequence and the third behave the same way.
    pub fn wait(&mut self, anchor: [f32; 4], icon: Option<String>, lines: Vec<Line>) {
        self.buttons.open_waiting(anchor);
        self.icon = icon;
        self.lines = wrap_notes(lines);
        self.wait = crate::menu::PRESS_TIME;
    }

    /// Advance the panel's growth by `dt` and return where it is now, 0 shut
    /// and 1 open.
    ///
    /// Stands still for as long as the row that opened it is still being
    /// watched going down. That hold is the whole reason this is not simply
    /// `self.buttons.animate`: the menu holds its ground for exactly that long
    /// before it starts folding into its own anchor, so a panel that grew from
    /// the moment of the press would grow *through* a menu still at full size —
    /// and, worse, take that menu's labels away as it covered them, leaving a
    /// row of naked chips behind. Waiting turns the two into one hand-off: the
    /// row goes down, and then the menu drops away as the panel comes out of
    /// the very row that was pressed.
    ///
    /// Only on the way in. A panel being dismissed is answering a press of its
    /// own and has nothing to wait for.
    pub fn animate(&mut self, dt: f32) -> f32 {
        if self.is_open() && self.wait > 0.0 {
            self.wait = (self.wait - dt).max(0.0);
            // Zero rather than skipping the call: the press bookkeeping inside
            // it still has to run, and it is what ends the press.
            return self.buttons.animate(0.0);
        }
        self.buttons.animate(dt)
    }

    /// Step into a further column of answers in place of the ones on the
    /// panel, once the button that asked for it has been seen to go down,
    /// opening on answer `start` — see [`Menu::descend_selecting`].
    ///
    /// For the one panel whose answers include a *value*: the kind of archive
    /// the Compress panel makes. The list of kinds takes the buttons' place
    /// under the same lines, so the panel grows by the difference rather than
    /// a second panel being raised over the first, and Back steps out to the
    /// answers it came from. The answers still never scroll — see
    /// [`Self::ask`] — which is what the window is widened for: a further
    /// list that is longer than the one it replaces is drawn whole.
    pub fn descend(&mut self, buttons: Vec<Entry>, start: usize) -> bool {
        let rows = buttons.len().max(self.buttons.entries().len());
        if !self.buttons.descend_selecting(None, buttons, start) {
            return false;
        }
        self.buttons.set_window(rows);
        true
    }

    /// Step back out to the answers a further column was reached from, once
    /// the answer just pressed has been seen to go down, with `buttons`
    /// written on them — see [`Menu::back_after_press`].
    pub fn back_after_press(&mut self, buttons: Vec<Entry>) {
        self.buttons.back_after_press(buttons);
    }

    /// Step back out to the answers a further column was reached from, now.
    /// `false` when the panel is showing the answers it opened with, which is
    /// what tells the caller that Back means closing it.
    pub fn back(&mut self) -> bool {
        self.buttons.back()
    }

    /// Put it away. Returns whether it was open, so a caller can tell a
    /// dismissal from a press that has to go on to mean something else.
    pub fn close(&mut self) -> bool {
        self.wait = 0.0;
        self.buttons.close()
    }

    /// Put it away once the answer just pressed has been seen to go down —
    /// see [`Menu::close_after_press`], and the one button that wants it.
    pub fn close_after_press(&mut self) -> bool {
        self.wait = 0.0;
        self.buttons.close_after_press()
    }

    /// Whether it is taking input.
    pub fn is_open(&self) -> bool {
        self.buttons.is_open()
    }

    /// Whether there is anything of it left to draw. Stays true while the panel
    /// falls back into the control it came out of.
    pub fn is_on_screen(&self) -> bool {
        self.buttons.is_on_screen()
    }

    /// Whether something is still moving, so the display it is on keeps drawing
    /// frames until it has settled — the hold before it starts growing
    /// included, since nothing else on this surface is necessarily moving
    /// through it.
    pub fn is_animating(&self) -> bool {
        self.wait > 0.0 || self.buttons.is_animating()
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Say how much of the foot of the display is taken by something else, in
    /// pixels, so the panel can centre itself in what is left.
    ///
    /// There is one thing that ever takes it: the on-screen keyboard, which
    /// comes up with the panel that asks for a password. A modal centred on the
    /// display would be half behind it.
    ///
    /// Told to the panel every frame rather than at the moment the keyboard
    /// opens, and for the same reason [`crate::menu::Menu::set_window`] is: the
    /// board *rises*, so the number changes on every one of those frames, and
    /// the panel lifting with it is what makes the two read as one movement
    /// rather than as a panel that jumped out of the way.
    pub fn set_footer(&mut self, pixels: f32) {
        self.footer = pixels.max(0.0);
    }

    pub fn footer(&self) -> f32 {
        self.footer
    }

    /// The application's icon, by theme name, drawn beside the heading.
    pub fn icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }

    /// Rewrite what the panel says without disturbing anything else about it.
    ///
    /// For a value that was not known when the panel opened and has since
    /// arrived — an application's version and size come back from a package
    /// manager a moment after the press. The buttons keep their highlight, the
    /// panel keeps its place in its flight, and the row simply stops saying
    /// "Reading…"; replacing the whole panel would restart both.
    pub fn say(&mut self, lines: Vec<Line>) {
        self.lines = wrap_notes(lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::Command;

    fn answers() -> Vec<Entry> {
        vec![
            Entry::new(Command::Dismiss, "No"),
            Entry::new(Command::ConfirmUninstall, "Yes").grave(),
        ]
    }

    fn asked() -> Dialog {
        let mut dialog = Dialog::default();
        assert!(dialog.ask(
            [10.0, 10.0, 40.0, 40.0],
            None,
            vec![Line::Note("Do you want to uninstall Example?".into())],
            answers(),
            0,
        ));
        dialog
    }

    /// The answer that declines is the one drawn first and the one the panel
    /// opens standing on. Both halves are asserted, because either alone lets
    /// the other drift: a question could open on its first answer and have the
    /// destroying one there, or put the harmless one first and start the
    /// highlight below it.
    #[test]
    fn a_question_opens_on_the_answer_that_declines_it() {
        let dialog = asked();
        assert!(dialog.is_open());
        assert_eq!(dialog.buttons.selected(), 0);
        assert_eq!(
            dialog.buttons.entries()[dialog.buttons.selected()].label,
            "No"
        );
        assert!(dialog.buttons.entries()[1].grave);
        assert!(!dialog.buttons.entries()[0].grave);
    }

    /// Choosing hands the keys back at once and the panel stays on screen,
    /// exactly as a menu row does — it is the same machinery underneath.
    #[test]
    fn an_answered_question_is_watched_before_the_panel_leaves() {
        let mut dialog = asked();
        while dialog.animate(0.05) < 1.0 {}

        assert_eq!(dialog.buttons.choose(), Some(Command::Dismiss));
        assert!(!dialog.is_open());
        assert!(dialog.is_on_screen());
        assert_eq!(dialog.animate(1.0 / 60.0), 1.0);

        dialog.buttons.backdate_press(crate::menu::PRESS_TIME);
        while dialog.animate(0.05) > 0.0 {}
        assert!(!dialog.is_on_screen());
    }

    /// The hand-off this exists for. The row that opened the panel is still
    /// being watched going down, and the menu holds its ground for exactly that
    /// long, so the panel must not start growing through a menu still at full
    /// size — it waits, and then the two move together.
    #[test]
    fn the_panel_waits_for_the_menu_to_start_leaving() {
        let mut dialog = asked();
        assert!(dialog.is_animating(), "the wait is still movement");

        let frame = 1.0 / 60.0;
        let mut waited = 0.0;
        while dialog.animate(frame) == 0.0 {
            waited += frame;
            assert!(waited < 1.0, "it never started growing");
        }
        assert!(
            (waited - crate::menu::PRESS_TIME).abs() <= frame,
            "it waited {waited}s, not the {}s the row takes to go down",
            crate::menu::PRESS_TIME
        );

        // Dismissed, it has nothing to wait for: the press it is answering is
        // its own.
        while dialog.animate(0.05) < 1.0 {}
        assert!(dialog.close());
        assert!(dialog.animate(frame) < 1.0);
    }

    /// The two panels overlap: the menu is still on screen at the moment the
    /// one it raised begins to grow, and neither leaves the screen empty of the
    /// other at any point in between.
    ///
    /// That overlap is what lets one push serve both. The start screen steps
    /// back while anything of the shell's stands over it — see
    /// [`crate::ui::recede_into_depth`] — and it holds the step for as long as
    /// something is there. A gap of even one frame between the menu going and
    /// the panel arriving would be the whole screen coming forward and going
    /// straight back again.
    #[test]
    fn the_menu_is_still_on_screen_while_the_panel_it_raised_grows() {
        let mut menu = Menu::default();
        assert!(menu.open_at(
            [10.0, 10.0, 40.0, 40.0],
            None,
            vec![Entry::new(Command::Information, "Information")],
            4,
        ));
        while menu.animate(0.05) < 1.0 {}
        assert_eq!(menu.choose(), Some(Command::Information));

        // What the shell does with that press, on the very same frame.
        let mut dialog = asked();
        assert!(
            dialog.is_on_screen(),
            "a panel counts from the moment it is asked for, not from the \
             moment it starts growing"
        );

        let frame = 1.0 / 60.0;
        let mut grew = false;
        for _ in 0..120 {
            // The row's press runs on the wall clock; the panels run on `dt`.
            menu.backdate_press(frame);
            let panel = dialog.animate(frame);
            menu.animate(frame);
            assert!(
                menu.is_on_screen() || dialog.is_on_screen(),
                "the screen behind is never left with nothing standing over it"
            );
            if panel > 0.0 && !grew {
                grew = true;
                assert!(
                    menu.is_on_screen(),
                    "the panel started growing after the menu had gone"
                );
            }
            if panel >= 1.0 {
                break;
            }
        }
        assert!(grew, "the panel never grew");
    }

    /// A value that arrives late replaces the line it was standing in for and
    /// nothing else: the panel does not restart, and the highlight does not
    /// move off the button the user had walked to.
    #[test]
    fn a_late_answer_rewrites_a_line_without_disturbing_the_panel() {
        let mut dialog = Dialog::default();
        dialog.ask(
            [0.0; 4],
            Some("example".into()),
            vec![Line::field("Version", "Reading…")],
            answers(),
            0,
        );
        while dialog.animate(0.05) < 1.0 {}
        dialog.buttons.move_selection(1);

        dialog.say(vec![Line::field("Version", "3.4.1-2")]);
        assert_eq!(dialog.lines(), [Line::field("Version", "3.4.1-2")]);
        assert_eq!(dialog.buttons.selected(), 1, "the highlight stayed put");
        assert_eq!(
            dialog.buttons.animate(1.0 / 60.0),
            1.0,
            "and so did the panel"
        );
        assert_eq!(dialog.icon(), Some("example"));
    }

    /// A panel with nothing pressable on it is a trap, so it never opens.
    #[test]
    fn a_panel_with_no_way_out_does_not_open() {
        let mut dialog = Dialog::default();
        assert!(!dialog.ask([0.0; 4], None, vec![Line::Rule], Vec::new(), 0));
        assert!(!dialog.is_open());
        assert!(!dialog.is_on_screen());
    }

    /// Unless it is a panel the shell is busy behind, which has nothing to
    /// press *yet*. It still takes every key — that is what makes it modal —
    /// and answers all of them with nothing.
    #[test]
    fn a_waiting_panel_takes_the_keys_and_answers_nothing() {
        let mut dialog = Dialog::default();
        dialog.wait(
            [10.0, 10.0, 40.0, 40.0],
            None,
            vec![Line::Note("Removing…".into())],
        );
        assert!(dialog.is_open(), "it has to hold the keys");
        assert!(dialog.is_on_screen());
        assert!(dialog.buttons.entries().is_empty());
        assert!(!dialog.buttons.move_selection(1));
        assert!(!dialog.buttons.move_selection(-1));
        assert_eq!(dialog.buttons.choose(), None);
        assert_eq!(
            dialog.buttons.selected(),
            0,
            "and never off the end of nothing"
        );

        // And it is replaced by an ordinary panel rather than dismissed, which
        // is a plain `ask` over the top of it.
        assert!(dialog.ask(
            [0.0; 4],
            None,
            vec![Line::Note("Removed.".into())],
            vec![Entry::new(Command::Dismiss, "OK")],
            0,
        ));
        assert_eq!(dialog.buttons.entries().len(), 1);
    }

    /// The panel centres itself in what the keyboard has left, and glides back
    /// to the middle as the board falls away.
    #[test]
    fn the_panel_makes_room_for_the_keyboard() {
        let mut dialog = asked();
        assert_eq!(dialog.footer(), 0.0);
        dialog.set_footer(320.0);
        assert_eq!(dialog.footer(), 320.0);
        // Never negative, however the caller worked it out.
        dialog.set_footer(-40.0);
        assert_eq!(dialog.footer(), 0.0);
    }
}
