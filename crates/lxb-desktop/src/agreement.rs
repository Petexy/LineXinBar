//! A Steam game's agreement, on the shell's own screen.
//!
//! Some games will not install until an agreement has been accepted — Garry's
//! Mod's terms of service, Black Mesa's licence, six titles in fifteen on one
//! real account. Valve's client asks that in a desktop window of its own, which
//! on a console is a window nobody can reach, so the install used to stop there
//! and offer to hand the whole thing to Steam.
//!
//! Now the press comes back with the agreements themselves
//! ([`lxb_steam::Stopped::Agreements`]) and they are put up here: the game's
//! name, one sentence saying what is being asked, the agreement's own words in
//! a well scrolled a screen at a time with Left and Right — the one way this
//! shell scrolls a panel, see [`crate::dialog::Line::Reading`] — and **Accept
//! and install** above **Not now**.
//!
//! A game with more than one is shown them one at a time, as Valve's own
//! dialog does, and the last one's button is the one that installs. Accepting
//! is recorded only then, all together, by the same call Valve's Accept button
//! makes — so leaving half-way through records nothing. An agreement whose
//! words could not be read is never offered for accepting: the panel says so
//! and offers to try again.

use crate::{dialog::Line, i18n, menu};
use lxb_steam::agreement::Agreement;

/// How many rows of an agreement are in the well at once.
///
/// With the heading, a two-line sentence, the rule and two answers, the panel
/// stands inside a 720-line display the way the updates' terminal does, and a
/// screen of an agreement is a screen of paragraphs rather than a peephole.
const ROWS: usize = 14;

/// The agreements one press stopped on, and how far the person has read.
#[derive(Debug)]
pub struct Agreements {
    /// The game that was pressed: the name at the head of the panel, and what
    /// is installed once every one of these is accepted.
    pub app_id: u32,
    list: Vec<Agreement>,
    /// Which one is on the screen.
    at: usize,
    /// Its words as the well's rows, wrapped once when it came on screen.
    rows: Vec<String>,
    /// The first of them in the well.
    top: usize,
    /// Made up by `--debug-actions agreement`, so that accepting it does
    /// nothing at all. See [`crate::model::Action::PretendAnAgreement`].
    pretend: bool,
}

impl Agreements {
    /// `None` for an empty list, which is not a question.
    pub fn new(app_id: u32, list: Vec<Agreement>) -> Option<Agreements> {
        if list.is_empty() {
            return None;
        }
        let mut agreements = Agreements {
            app_id,
            list,
            at: 0,
            rows: Vec::new(),
            top: 0,
            pretend: false,
        };
        agreements.wrap();
        Some(agreements)
    }

    fn current(&self) -> &Agreement {
        &self.list[self.at]
    }

    fn wrap(&mut self) {
        self.rows = self
            .current()
            .text
            .as_deref()
            .map(crate::gpu::wrap_reading)
            .unwrap_or_default();
        self.top = 0;
    }

    /// Whether the one on the screen can be read, and so accepted.
    pub fn readable(&self) -> bool {
        self.current().text.is_some()
    }

    /// Move the well a screen through the words, less a row so that nothing
    /// is read past. Says whether it moved.
    pub fn scroll(&mut self, forward: bool) -> bool {
        let last = self.rows.len().saturating_sub(ROWS);
        let step = ROWS.saturating_sub(1).max(1);
        let to = if forward {
            self.top.saturating_add(step).min(last)
        } else {
            self.top.saturating_sub(step)
        };
        let moved = to != self.top;
        self.top = to;
        moved
    }

    /// Step on to the next agreement, having accepted this one. `false` when
    /// this was the last.
    pub fn advance(&mut self) -> bool {
        if self.at + 1 >= self.list.len() {
            return false;
        }
        self.at += 1;
        self.wrap();
        true
    }

    /// Everything that was on offer, to be recorded as accepted. Asked only
    /// once the last of them has been pressed Accept on.
    pub fn accepted(&self) -> Vec<lxb_steam::webui::Eula> {
        self.list
            .iter()
            .map(|agreement| agreement.eula.clone())
            .collect()
    }

    /// What the panel says, under the game's name.
    pub fn lines(&self, game: &str) -> Vec<Line> {
        let mut lines = vec![Line::Heading(game.to_owned())];
        if !self.readable() {
            lines.push(Line::Note(crate::message!(
                "steam-agreement-could-not-be-loaded",
                "game" => game
            )));
            lines.push(Line::Rule);
            return lines;
        }
        lines.push(Line::Note(crate::message!(
            "steam-agreement-before-install",
            "game" => game,
            "at" => self.at + 1,
            "total" => self.list.len()
        )));
        let shown: Vec<String> = self
            .rows
            .iter()
            .skip(self.top)
            .take(ROWS)
            .cloned()
            .collect();
        let foot = if self.rows.len() <= ROWS {
            String::new()
        } else {
            crate::message!(
                "terminal-lines-scroll",
                "from" => self.top + 1,
                "to" => (self.top + ROWS).min(self.rows.len()),
                "total" => self.rows.len()
            )
        };
        lines.push(Line::Reading {
            lines: shown,
            rows: ROWS,
            foot,
        });
        lines.push(Line::Rule);
        lines
    }

    /// What can be pressed. Accept first and chosen first, because it is the
    /// verb the person came for — they pressed Install — and it is not a press
    /// that destroys anything; the way out is under it and says what leaving
    /// does.
    pub fn buttons(&self) -> Vec<menu::Entry> {
        let first = if !self.readable() {
            menu::Entry::new(
                menu::Command::SteamInstall(self.app_id),
                i18n::text("shell-try-again"),
            )
        } else if self.at + 1 < self.list.len() {
            // Stays up: the next agreement takes this one's place on the
            // same panel.
            menu::Entry::new(
                menu::Command::SteamAcceptAgreement(self.app_id),
                i18n::text("steam-accept-agreement"),
            )
            .holds()
        } else {
            menu::Entry::new(
                menu::Command::SteamAcceptAgreement(self.app_id),
                i18n::text("steam-accept-and-install"),
            )
        };
        vec![
            first,
            menu::Entry::new(menu::Command::Dismiss, i18n::text("shell-not-now")),
        ]
    }
}

impl crate::Shell {
    /// A press on a game came back with agreements to accept first: put the
    /// first of them up.
    pub(crate) fn offer_the_agreements(&mut self, app_id: u32, list: Vec<Agreement>) {
        let Some(agreements) = Agreements::new(app_id, list) else {
            return;
        };
        self.agreements = Some(agreements);
        self.show_the_agreement(true);
    }

    /// Put two invented agreements up, for looking at the panel — see
    /// [`crate::model::Action::PretendAnAgreement`]. For Garry's Mod, which is
    /// the game this was written for; a session whose library has not got it
    /// heads the panel with its number instead.
    pub(crate) fn pretend_an_agreement(&mut self) {
        tracing::warn!(
            "--debug-actions agreement: these agreements are invented, and accepting them records and installs nothing"
        );
        let invented = |id: &str, title: &str, paragraphs: usize| Agreement {
            eula: lxb_steam::webui::Eula {
                app_id: 4000,
                id: id.to_string(),
                version: 1,
                url: String::new(),
            },
            title: title.to_string(),
            text: Some(
                std::iter::once(title.to_uppercase())
                    .chain((1..=paragraphs).map(|n| {
                        format!(
                            "{n}. This paragraph is invented, and so is the agreement it is part \
                             of. It is here so that the panel an agreement is read on has \
                             something long enough to scroll through, written at about the \
                             length a publisher writes a clause at, so that the rows wrap the \
                             way a real agreement's do."
                        )
                    }))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
        };
        let Some(mut agreements) = Agreements::new(
            4000,
            vec![
                invented("pretend_eula_0", "Invented terms of service", 24),
                invented("pretend_eula_1", "Invented privacy notice", 3),
            ],
        ) else {
            return;
        };
        agreements.pretend = true;
        self.agreements = Some(agreements);
        self.show_the_agreement(true);
    }

    /// Draw the agreement panel: raised afresh, or written over in place where
    /// it is already standing — a scroll, or the next agreement.
    fn show_the_agreement(&mut self, raise: bool) {
        let Some(agreements) = &self.agreements else {
            return;
        };
        let app_id = agreements.app_id;
        let name = self
            .steam
            .game(app_id)
            .map(|game| game.name.clone())
            .unwrap_or_else(|| crate::message!("steam-app-number", "app" => app_id));
        let lines = agreements.lines(&name);
        let buttons = agreements.buttons();
        if raise {
            let from = self.dialog_origin();
            self.dialog.ask(
                from,
                Some(crate::icons::STEAM.to_string()),
                lines,
                buttons,
                0,
            );
        } else {
            self.dialog.say(lines);
            self.dialog.refresh(buttons);
        }
        self.needs_redraw = true;
    }

    /// Whether the panel on the screen is an agreement being read.
    pub(crate) fn agreement_is_up(&self) -> bool {
        let Some(agreements) = &self.agreements else {
            return false;
        };
        self.dialog.is_open()
            && self.dialog.buttons.entries().iter().any(|entry| {
                entry.command == menu::Command::SteamAcceptAgreement(agreements.app_id)
            })
    }

    /// Scroll the agreement on the screen, if there is one. Says whether the
    /// press was the agreement's, so it is not spent anywhere else.
    pub(crate) fn scroll_the_agreement(&mut self, forward: bool) -> bool {
        if !self.agreement_is_up() {
            return false;
        }
        if self
            .agreements
            .as_mut()
            .is_some_and(|agreements| agreements.scroll(forward))
        {
            self.stepped();
            self.show_the_agreement(false);
        }
        true
    }

    /// Accept pressed on the agreement on the screen: the next one, or — the
    /// last of them accepted — record them all and fetch the game.
    pub(crate) fn accept_the_agreement(&mut self, app_id: u32) {
        let Some(agreements) = self
            .agreements
            .as_mut()
            .filter(|agreements| agreements.app_id == app_id && agreements.readable())
        else {
            tracing::warn!(
                app_id,
                "Accept was pressed with no agreement for it on the screen"
            );
            return;
        };
        if agreements.advance() {
            self.show_the_agreement(false);
            return;
        }
        let accepting = agreements.accepted();
        let pretend = agreements.pretend;
        self.agreements = None;
        if pretend {
            tracing::warn!(
                "--debug-actions agreement: accepted, and nothing was recorded or installed"
            );
            return;
        }
        tracing::info!(
            app_id,
            agreements = ?accepting.iter().map(|eula| &eula.id).collect::<Vec<_>>(),
            "the agreements were accepted on the shell's panel; fetching the game"
        );
        self.steam.accept_and_install(app_id, accepting);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxb_steam::webui::Eula;

    fn agreement(id: &str, text: Option<&str>) -> Agreement {
        Agreement {
            eula: Eula {
                app_id: 4000,
                id: id.to_string(),
                version: 1,
                url: format!("https://store.steampowered.com/eula/{id}"),
            },
            title: "Terms".to_string(),
            text: text.map(str::to_string),
        }
    }

    fn long_text() -> String {
        (1..=60)
            .map(|n| format!("Paragraph {n}."))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn reading(lines: &[Line]) -> (&[String], &str) {
        lines
            .iter()
            .find_map(|line| match line {
                Line::Reading { lines, foot, .. } => Some((lines.as_slice(), foot.as_str())),
                _ => None,
            })
            .expect("a reading well")
    }

    /// A long agreement is read a screen at a time, a row of the last screen
    /// kept at the top of the next, and never past either end.
    #[test]
    fn an_agreement_is_read_a_screen_at_a_time() {
        let mut agreements =
            Agreements::new(4000, vec![agreement("4000_eula_0", Some(&long_text()))]).unwrap();
        let first = agreements.lines("Garry's Mod");
        let (shown, foot) = reading(&first);
        assert_eq!(shown.len(), ROWS);
        assert_eq!(shown[0], "Paragraph 1.");
        assert!(
            foot.contains("1") && foot.contains(&agreements.rows.len().to_string()),
            "{foot}"
        );

        assert!(!agreements.scroll(false), "nothing before the top");
        assert!(agreements.scroll(true));
        let second = agreements.lines("Garry's Mod");
        let (shown, _) = reading(&second);
        let (before, _) = reading(&first);
        assert_eq!(shown[0], before[ROWS - 1], "a row carried over");

        while agreements.scroll(true) {}
        let lines = agreements.lines("Garry's Mod");
        let (shown, _) = reading(&lines);
        assert_eq!(shown.last().map(String::as_str), Some("Paragraph 60."));
        assert!(!agreements.scroll(true), "nothing past the end");
    }

    /// A short one has nothing to scroll and says nothing about scrolling.
    #[test]
    fn a_short_agreement_has_no_scrolling_to_explain() {
        // Grand Theft Auto V's, which is a link and nothing else.
        let link = "https://www.rockstargames.com/legal?country=pl";
        let mut agreements = Agreements::new(4000, vec![agreement("a", Some(link))]).unwrap();
        let lines = agreements.lines("Grand Theft Auto V");
        let (shown, foot) = reading(&lines);
        assert_eq!(shown, [link.to_string()]);
        assert!(foot.is_empty());
        assert!(!agreements.scroll(true));
    }

    /// Two agreements are two panels: the first one's button holds the panel
    /// for the second, and only the last one's installs. Everything is
    /// recorded together, once, at the end.
    #[test]
    fn several_agreements_are_accepted_one_after_another() {
        let mut agreements = Agreements::new(
            4000,
            vec![
                agreement("one", Some("First.")),
                agreement("two", Some("Second.")),
            ],
        )
        .unwrap();
        let buttons = agreements.buttons();
        assert_eq!(
            buttons[0].command,
            menu::Command::SteamAcceptAgreement(4000)
        );
        assert!(
            buttons[0].holds,
            "the next agreement takes the panel's place"
        );
        assert_eq!(buttons[1].command, menu::Command::Dismiss);

        assert!(agreements.advance());
        let lines = agreements.lines("Game");
        let (shown, _) = reading(&lines);
        assert_eq!(shown, ["Second.".to_string()]);
        let buttons = agreements.buttons();
        assert_eq!(
            buttons[0].command,
            menu::Command::SteamAcceptAgreement(4000)
        );
        assert!(
            !buttons[0].holds,
            "the last Accept installs and the panel goes"
        );
        assert!(!agreements.advance());
        assert_eq!(
            agreements
                .accepted()
                .iter()
                .map(|eula| eula.id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    /// An agreement whose words could not be read cannot be accepted: no
    /// Accept, no well, a sentence and a way to try again.
    #[test]
    fn an_agreement_nobody_can_read_is_not_offered_for_accepting() {
        let agreements = Agreements::new(4000, vec![agreement("a", None)]).unwrap();
        assert!(!agreements.readable());
        let lines = agreements.lines("Garry's Mod");
        assert!(!lines
            .iter()
            .any(|line| matches!(line, Line::Reading { .. })));
        let commands: Vec<_> = agreements
            .buttons()
            .iter()
            .map(|entry| entry.command)
            .collect();
        assert_eq!(
            commands,
            [menu::Command::SteamInstall(4000), menu::Command::Dismiss]
        );
        assert!(Agreements::new(4000, Vec::new()).is_none());
    }
}
