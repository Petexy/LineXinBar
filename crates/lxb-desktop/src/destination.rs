//! Where a Steam game goes, asked on the shell's own screen.
//!
//! On a machine with more than one Steam library, Valve's client asks which one
//! a game goes into, in a desktop window of its own — the folder list in its
//! install dialog. On a console that window is nowhere anybody can reach, and
//! the shell used to answer it on the person's behalf by going on with
//! whatever Steam's default was.
//!
//! Now the press comes back with the libraries themselves and how much room the
//! game needs ([`lxb_steam::Stopped::WhereTo`]), and they are put up here: the
//! game's name, one sentence, and a button per library saying how much room is
//! left on it — the one a game will not fit on is there, greyed out and saying
//! so, because a drive that has vanished from the list is a question in
//! itself. Pressing one installs into it; **Not now** is under them.
//!
//! Asked only when Settings > Games > Steam > Install games to says so, which
//! it does unless somebody chose a library there — and asked then as well
//! when the one they chose is not plugged in, or is too full, with the reason
//! at the top of the panel. See [`crate::settings::SteamValue::InstallTo`].

use crate::{dialog::Line, i18n, menu};
use lxb_steam::webui::{Choice, Place};

/// Why the library the settings name is not simply used.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Instead {
    /// It is not one of the libraries that are there today.
    NotThere(String),
    /// It is there, and the game does not fit.
    NoRoom(String),
}

/// The libraries one press stopped on.
#[derive(Debug)]
pub struct Destinations {
    /// The game that was pressed: the name at the head of the panel, and what
    /// is installed into whichever library is chosen.
    pub app_id: u32,
    choice: Choice,
    /// What each library is called on its button, in the choice's order.
    names: Vec<String>,
    instead: Option<Instead>,
    /// Made up by `--debug-actions library`, so that choosing one installs
    /// nothing at all. See [`crate::model::Action::PretendALibraryChoice`].
    pretend: bool,
}

impl Destinations {
    /// `None` for a choice with nothing in it, which is not a question.
    ///
    /// `named` is the library the press was sent to, where it was sent to one:
    /// it is why the panel is up at all, and the panel says so.
    pub fn new(app_id: u32, choice: Choice, named: Option<&str>) -> Option<Destinations> {
        if choice.libraries.is_empty() {
            return None;
        }
        let names = crate::settings::steam_library_names(
            &choice
                .libraries
                .iter()
                .map(|library| (library.path.as_str(), library.label.as_str()))
                .collect::<Vec<_>>(),
        );
        let instead = named.map(|path| {
            match choice
                .libraries
                .iter()
                .position(|library| crate::settings::same_library(&library.path, path))
            {
                Some(at) => Instead::NoRoom(names[at].clone()),
                None => Instead::NotThere(what_the_settings_call(path)),
            }
        });
        Some(Destinations {
            app_id,
            choice,
            names,
            instead,
            pretend: false,
        })
    }

    /// What the panel says, under the game's name.
    pub fn lines(&self, game: &str) -> Vec<Line> {
        let mut lines = vec![Line::Heading(game.to_owned())];
        let size = (self.choice.needs > 0).then(|| crate::steam::format_size(self.choice.needs));
        let anywhere = self
            .choice
            .libraries
            .iter()
            .any(|library| library.fits(self.choice.needs));
        match &self.instead {
            Some(Instead::NotThere(name)) => lines.push(Line::Note(crate::message!(
                "steam-chosen-library-not-connected",
                "library" => name.as_str()
            ))),
            Some(Instead::NoRoom(name)) => lines.push(Line::Note(crate::message!(
                "steam-chosen-library-full",
                "library" => name.as_str()
            ))),
            None => {}
        }
        lines.push(Line::Note(match (&size, anywhere, &self.instead) {
            (Some(size), false, _) => {
                crate::message!("steam-no-library-has-room", "size" => size.as_str())
            }
            (Some(size), true, Some(_)) => {
                crate::message!("steam-game-needs", "size" => size.as_str())
            }
            (Some(size), true, None) => {
                crate::message!("steam-choose-library-size", "size" => size.as_str())
            }
            (None, _, _) => i18n::text("steam-choose-library").to_string(),
        }));
        lines.push(Line::Rule);
        lines
    }

    /// One button per library, then **Not now**.
    ///
    /// A library the game will not fit on is a button that cannot be pressed
    /// rather than a missing one, and says why on itself: a drive that was
    /// there yesterday and is not in the list today is a question, and one
    /// greyed out with "not enough space" on it answers it.
    pub fn buttons(&self) -> Vec<menu::Entry> {
        let needs = self.choice.needs;
        let mut buttons: Vec<menu::Entry> = self
            .choice
            .libraries
            .iter()
            .zip(&self.names)
            .enumerate()
            .map(|(at, (library, name))| {
                let fits = library.fits(needs);
                let label = match fits {
                    true => crate::message!(
                        "steam-library-free",
                        "library" => name.as_str(),
                        "free" => crate::steam::format_size(library.free)
                    ),
                    false => crate::message!(
                        "steam-library-too-small",
                        "library" => name.as_str()
                    ),
                };
                let entry = menu::Entry::new(
                    menu::Command::SteamInstallInto {
                        app_id: self.app_id,
                        library: u8::try_from(at).unwrap_or(u8::MAX),
                    },
                    label,
                )
                .glyph(crate::icons::FILE_DRIVE);
                match fits {
                    true => entry,
                    false => entry.disabled(),
                }
            })
            .collect();
        buttons.push(menu::Entry::new(
            menu::Command::Dismiss,
            i18n::text("shell-not-now"),
        ));
        buttons
    }

    /// Which button the cursor starts on: Steam's own default where the game
    /// fits there, which is where Valve's dialog would have started; else the
    /// first library it fits on; and **Not now** where it fits on none.
    pub fn start(&self) -> usize {
        let needs = self.choice.needs;
        let libraries = &self.choice.libraries;
        libraries
            .iter()
            .position(|library| library.default && library.fits(needs))
            .or_else(|| libraries.iter().position(|library| library.fits(needs)))
            .unwrap_or(libraries.len())
    }

    /// Where the library behind one button is, if that button is one and the
    /// game fits there.
    pub fn chosen(&self, library: u8) -> Option<&str> {
        self.choice
            .libraries
            .get(usize::from(library))
            .filter(|library| library.fits(self.choice.needs))
            .map(|library| library.path.as_str())
    }
}

/// What a library the settings name is called where the client has not listed
/// it — before a press, or when it is not there: Steam's own name for it off
/// the disk, else the drive it is on.
pub(crate) fn what_the_settings_call(path: &str) -> String {
    let label = lxb_steam::backend::Backend::chosen()
        .map(|backend| backend.listed_libraries())
        .unwrap_or_default()
        .into_iter()
        .find(|listed| crate::settings::same_library(&listed.path, path))
        .map(|listed| listed.label)
        .unwrap_or_default();
    crate::settings::steam_library_names(&[(path, label.as_str())])
        .pop()
        .unwrap_or_else(|| path.to_string())
}

impl crate::Shell {
    /// A press on a game came back asking where it should go: put the
    /// libraries up.
    ///
    /// `named` is where the press was sent, read back from what it was sent
    /// with — see [`crate::steam::Steam::place`].
    pub(crate) fn offer_the_libraries(&mut self, app_id: u32, choice: Choice) {
        let named = match self.steam.place(app_id) {
            Some(Place::In { path, .. }) => Some(path),
            _ => None,
        };
        let Some(destinations) = Destinations::new(app_id, choice, named.as_deref()) else {
            return;
        };
        self.destinations = Some(destinations);
        self.show_the_libraries();
    }

    /// Put three invented libraries up, for looking at the panel — see
    /// [`crate::model::Action::PretendALibraryChoice`]. One of them too small
    /// for the game, so the greyed-out button can be seen as well.
    pub(crate) fn pretend_a_library_choice(&mut self) {
        tracing::warn!(
            "--debug-actions library: these libraries are invented, and choosing one installs nothing"
        );
        const GIB: u64 = 1024 * 1024 * 1024;
        let invented =
            |path: &str, label: &str, free: u64, default: bool| lxb_steam::webui::Library {
                path: path.to_string(),
                label: label.to_string(),
                free: free * GIB,
                default,
            };
        let choice = Choice {
            needs: 46 * GIB,
            libraries: vec![
                invented("/home/pretend/.local/share/Steam", "", 31, true),
                invented("/mnt/pretend/SteamLibrary", "Games", 412, false),
                invented("/run/media/pretend/SteamLibrary", "USB drive", 118, false),
            ],
        };
        let Some(mut destinations) = Destinations::new(4000, choice, None) else {
            return;
        };
        destinations.pretend = true;
        self.destinations = Some(destinations);
        self.show_the_libraries();
    }

    fn show_the_libraries(&mut self) {
        let Some(destinations) = &self.destinations else {
            return;
        };
        let app_id = destinations.app_id;
        let name = self
            .steam
            .game(app_id)
            .map(|game| game.name.clone())
            .unwrap_or_else(|| crate::message!("steam-app-number", "app" => app_id));
        let lines = destinations.lines(&name);
        let buttons = destinations.buttons();
        let start = destinations.start();
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(crate::icons::STEAM.to_string()),
            lines,
            buttons,
            start,
        );
        self.needs_redraw = true;
    }

    /// One library pressed on that panel: install the game into it.
    ///
    /// Into it and nowhere else, and without making it Steam's default: this
    /// is one game's answer. The standing answer is the settings row's.
    pub(crate) fn install_into(&mut self, app_id: u32, library: u8) {
        let Some(destinations) = self
            .destinations
            .take()
            .filter(|destinations| destinations.app_id == app_id)
        else {
            tracing::warn!(
                app_id,
                "a library was pressed with no choice for it on the screen"
            );
            return;
        };
        let Some(path) = destinations.chosen(library).map(str::to_string) else {
            tracing::warn!(
                app_id,
                library,
                "that library is not one the game can go into"
            );
            return;
        };
        if destinations.pretend {
            tracing::warn!(%path, "--debug-actions library: chosen, and nothing was installed");
            return;
        }
        tracing::info!(app_id, %path, "a library was chosen on the shell's panel; fetching the game");
        self.steam.install(
            app_id,
            Place::In {
                path,
                by_default: false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxb_steam::webui::Library;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn library(path: &str, free: u64, default: bool) -> Library {
        Library {
            path: path.to_string(),
            label: path.trim_start_matches('/').to_string(),
            free: free * GIB,
            default,
        }
    }

    fn choice(needs: u64, libraries: Vec<Library>) -> Choice {
        Choice {
            needs: needs * GIB,
            libraries,
        }
    }

    fn commands(destinations: &Destinations) -> Vec<(menu::Command, bool)> {
        destinations
            .buttons()
            .iter()
            .map(|entry| (entry.command, entry.enabled))
            .collect()
    }

    /// One button per library in Steam's order, then Not now — and the one
    /// the game will not fit on is there and cannot be pressed.
    #[test]
    fn every_library_is_a_button_and_a_full_one_cannot_be_pressed() {
        let destinations = Destinations::new(
            4000,
            choice(
                40,
                vec![library("/small", 31, true), library("/big", 412, false)],
            ),
            None,
        )
        .unwrap();
        let into = |library| menu::Command::SteamInstallInto {
            app_id: 4000,
            library,
        };
        assert_eq!(
            commands(&destinations),
            [
                (into(0), false),
                (into(1), true),
                (menu::Command::Dismiss, true)
            ]
        );
        assert_eq!(destinations.chosen(0), None, "a full drive is not a choice");
        assert_eq!(destinations.chosen(1), Some("/big"));
        assert_eq!(destinations.chosen(7), None);
    }

    /// The cursor starts where Valve's dialog would — Steam's default — when
    /// the game fits there, on the first library it fits on when it does
    /// not, and on Not now when it fits nowhere.
    #[test]
    fn the_cursor_starts_on_the_default_where_the_game_fits() {
        let start = |needs, libraries| {
            Destinations::new(1, choice(needs, libraries), None)
                .unwrap()
                .start()
        };
        let two = |default_is_big: bool| {
            vec![
                library("/small", 31, !default_is_big),
                library("/big", 412, default_is_big),
            ]
        };
        assert_eq!(start(10, two(false)), 0);
        assert_eq!(start(10, two(true)), 1);
        assert_eq!(start(40, two(false)), 1, "the default is full");
        assert_eq!(start(500, two(false)), 2, "nothing fits: Not now");
    }

    /// The panel says why it is up when the press named a library: not
    /// connected, or full. And a game that fits nowhere is said to, with how
    /// much it needs.
    #[test]
    fn the_panel_says_why_the_chosen_library_was_not_used() {
        let two = || vec![library("/small", 31, true), library("/big", 412, false)];
        let notes = |destinations: &Destinations| -> Vec<String> {
            destinations
                .lines("Game")
                .into_iter()
                .filter_map(|line| match line {
                    Line::Note(note) => Some(note),
                    _ => None,
                })
                .collect()
        };

        let full = Destinations::new(1, choice(40, two()), Some("/small/")).unwrap();
        assert_eq!(full.instead, Some(Instead::NoRoom("small".to_string())));
        assert_eq!(notes(&full).len(), 2);

        let asked = Destinations::new(1, choice(40, two()), None).unwrap();
        assert_eq!(asked.instead, None);
        assert_eq!(notes(&asked).len(), 1);

        let nowhere = Destinations::new(1, choice(500, two()), None).unwrap();
        assert_eq!(nowhere.start(), 2);
        assert_ne!(notes(&nowhere), notes(&asked));

        assert!(Destinations::new(1, choice(1, Vec::new()), None).is_none());
    }
}
