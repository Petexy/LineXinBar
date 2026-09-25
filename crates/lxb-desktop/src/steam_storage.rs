//! Settings > Games > Steam > Storage, the shell's half of it: what is in each
//! library, read off the disk, and the panels a press on that page puts up.
//!
//! The page itself is written in [`crate::settings`] with the rest of the
//! tree. What it copies is Valve's own storage page — the libraries, the games
//! in each, **Add Drive** — and every change it makes is made the way that page
//! makes it, through Valve's client: see [`lxb_steam::webui::shelve`] and
//! [`lxb_steam::webui::move_game`]. Nothing here writes Steam's files.
//!
//! Four panels, all in the shape the rest of the shell's Steam panels have:
//!
//! - **Where to move a game**: the game, how much room it needs, a button per
//!   other library saying how much room is left there — greyed out and saying
//!   so where it will not fit — and **Not now**. The shape of the panel that
//!   asks where a game is installed, on purpose ([`crate::destination`]).
//! - **Moving**: the game, where it is going and a bar with the per cent on it,
//!   with **Hide** and **Stop**. Hidden, the move goes on and the game's row on
//!   the page carries the bar; the end is announced in the corner.
//! - **Remove drive?**, asked before a library is forgotten.
//! - **One moment**, while a library is added, removed or repaired, which ends
//!   in the page changing under it or in a sentence saying why it did not.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::dialog::Line;
use crate::settings::{MovingNow, StoredGame};
use crate::{apps, i18n, icons, menu, settings};
use lxb_steam::library::{Listed, Standing};
use lxb_steam::webui::{Declined, Moved, Shelving};
use lxb_steam::StorageRefused;

/// Steam's libraries, the folder it is installed in, and the games in each, as
/// the disk says now.
#[derive(Debug, Default)]
pub(crate) struct Read {
    pub listed: Vec<Listed>,
    pub root: Option<String>,
    pub games: BTreeMap<String, Vec<StoredGame>>,
}

/// Read them, from the Steam this session drives.
///
/// Small files — `libraryfolders.vdf` and one manifest per game — and read only
/// when the page they are drawn on is arrived at or watched, or when something
/// this session asked of Steam has just changed them.
pub(crate) fn read() -> Read {
    let Some(backend) = lxb_steam::backend::Backend::chosen() else {
        return Read::default();
    };
    let listed = backend.listed_libraries();
    let games = listed
        .iter()
        .filter(|library| library.present)
        .map(|library| (library.path.clone(), games_in(&library.path)))
        .collect();
    Read {
        listed,
        root: Some(backend.root().to_string_lossy().into_owned()),
        games,
    }
}

/// Every game in one library, largest first — Steam's own order on its
/// storage page, and the order somebody freeing up room reads in.
///
/// Every one its manifests list, Steam's own runtimes and Proton included:
/// they take room like anything else, and Valve's page lists them.
fn games_in(path: &str) -> Vec<StoredGame> {
    let mut games: Vec<StoredGame> = lxb_steam::library::installed_in(&[PathBuf::from(path)])
        .into_values()
        .map(|installed| StoredGame {
            app_id: installed.app_id,
            name: installed.name,
            size: installed.size_on_disk,
            busy: busy(installed.standing),
        })
        .collect();
    games.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    games
}

/// Whether Steam is in the middle of something with a game, so that it may be
/// neither moved nor taken off from here. The three it is not doing anything
/// with are the three the game's own menu offers Uninstall on.
fn busy(standing: Standing) -> bool {
    !matches!(
        standing,
        Standing::Ready | Standing::UpdatePaused | Standing::Broken
    )
}

/// The menu over one game on a library's page.
///
/// **Move** where there is another library to move it to, and the way to the
/// move's own panel instead while this game is the one moving; **Uninstall**;
/// and the way out. Both greyed out while Steam is working on the game — the
/// row says so — because either would be a second hand on files Steam has in
/// its own.
pub(crate) fn stored_menu_rows(
    stored: &apps::Stored,
    elsewhere: bool,
    moving: Option<u32>,
) -> Vec<menu::Entry> {
    let mut rows = Vec::new();
    let this_one_is_moving = moving == Some(stored.app_id);
    if this_one_is_moving {
        rows.push(
            menu::Entry::new(
                menu::Command::SteamShowMove,
                i18n::text("steam-move-progress"),
            )
            .glyph(icons::MOVE),
        );
    } else if elsewhere {
        let entry = menu::Entry::new(
            menu::Command::SteamMoveGame(stored.app_id),
            i18n::text("shell-move"),
        )
        .glyph(icons::MOVE);
        rows.push(match stored.busy {
            true => entry.disabled(),
            false => entry,
        });
    }
    let uninstall = menu::Entry::new(
        menu::Command::SteamUninstall(stored.app_id),
        i18n::text("shell-uninstall"),
    )
    .glyph(icons::UNINSTALL)
    .grave();
    rows.push(match stored.busy || this_one_is_moving {
        true => uninstall.disabled(),
        false => uninstall,
    });
    rows.push(menu::Entry::new(menu::Command::Dismiss, i18n::text("shell-cancel")).group(1));
    rows
}

/// Which of those libraries can a game of this size go into.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    path: String,
    name: String,
    /// Room left on its drive, where Settings > Storage has read it.
    free: Option<u64>,
    fits: bool,
}

/// The libraries one game could be moved into, on the panel that asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoveChoice {
    app_id: u32,
    name: String,
    /// How much room it takes where it is, which is what it needs where it
    /// goes.
    needs: u64,
    places: Vec<Place>,
}

impl MoveChoice {
    /// Every library but the one it is in and the ones not plugged in.
    ///
    /// A library fits where there is more room left than the game takes, even
    /// on the drive it is already on: Valve's client copies a game's files
    /// across and counts the bytes as it goes, so the room is wanted whether or
    /// not the drive is the same — and a button reading "27 GB free" under "It
    /// needs 34 GB" must not be the one that can be pressed. Where the room on
    /// a drive is not known, the move is offered and Steam is the one to say no.
    fn new(
        app_id: u32,
        name: String,
        needs: u64,
        from: &str,
        listed: &[Listed],
        room: impl Fn(&str) -> Option<u64>,
    ) -> MoveChoice {
        let others: Vec<&Listed> = listed
            .iter()
            .filter(|library| library.present && !settings::same_library(&library.path, from))
            .collect();
        let names = settings::steam_library_names(
            &others
                .iter()
                .map(|library| (library.path.as_str(), library.label.as_str()))
                .collect::<Vec<_>>(),
        );
        let places = others
            .iter()
            .zip(names)
            .map(|(library, name)| {
                let free = room(&library.path);
                Place {
                    fits: free.is_none_or(|free| free > needs),
                    path: library.path.clone(),
                    name,
                    free,
                }
            })
            .collect();
        MoveChoice {
            app_id,
            name,
            needs,
            places,
        }
    }

    fn lines(&self) -> Vec<Line> {
        let size = crate::steam::format_size(self.needs);
        let note = match (
            self.places.is_empty(),
            self.places.iter().any(|place| place.fits),
        ) {
            (true, _) => i18n::text("steam-move-no-other-library").to_string(),
            (false, false) => crate::message!("steam-move-nowhere", "size" => size.as_str()),
            (false, true) => crate::message!("steam-move-choose", "size" => size.as_str()),
        };
        vec![
            Line::Heading(self.name.clone()),
            Line::Note(note),
            Line::Rule,
        ]
    }

    /// A button per library, then **Not now** — the one it will not fit on
    /// greyed out and saying so.
    fn buttons(&self) -> Vec<menu::Entry> {
        let mut buttons: Vec<menu::Entry> = self
            .places
            .iter()
            .enumerate()
            .map(|(at, place)| {
                let label = match (place.fits, place.free) {
                    (false, _) => {
                        crate::message!("steam-library-too-small", "library" => place.name.as_str())
                    }
                    (true, Some(free)) => crate::message!(
                        "steam-library-free",
                        "library" => place.name.as_str(),
                        "free" => crate::steam::format_size(free)
                    ),
                    (true, None) => place.name.clone(),
                };
                let entry = menu::Entry::new(
                    menu::Command::SteamMoveInto {
                        app_id: self.app_id,
                        library: u8::try_from(at).unwrap_or(u8::MAX),
                    },
                    label,
                )
                .glyph(icons::FILE_DRIVE);
                match place.fits {
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

    /// Where the cursor starts: the first library it fits in, else Not now.
    fn start(&self) -> usize {
        self.places
            .iter()
            .position(|place| place.fits)
            .unwrap_or(self.places.len())
    }

    /// The library behind one button, if it is one and the game fits there.
    fn chosen(&self, library: u8) -> Option<&Place> {
        self.places
            .get(usize::from(library))
            .filter(|place| place.fits)
    }
}

/// Which of this page's panels is the one on screen, where one is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Panel {
    /// The move of this game, with its bar.
    Moving(u32),
    /// A library being added, removed or repaired.
    Shelving(Shelving),
}

/// What the move panel says: the game, where it is going and how far along,
/// as a bar — or the lights until Steam has said, and while it is stopping.
fn moving_lines(name: &str, moving: &MovingNow) -> Vec<Line> {
    let library = settings::steam_library_name(&moving.to);
    let (note, bar) = match (moving.stopping, moving.percent) {
        (true, _) => (
            i18n::text("steam-moving-stopping").to_string(),
            Line::Waiting,
        ),
        (false, Some(percent)) => (
            crate::message!(
                "steam-moving-to-percent",
                "library" => library.as_str(),
                "percent" => percent
            ),
            Line::Progress(percent),
        ),
        (false, None) => (
            crate::message!("steam-moving-to", "library" => library.as_str()),
            Line::Waiting,
        ),
    };
    vec![
        Line::Heading(name.to_string()),
        Line::Note(note),
        bar,
        Line::Rule,
    ]
}

/// **Hide**, and **Stop** until it has been pressed.
fn moving_buttons(moving: &MovingNow) -> Vec<menu::Entry> {
    let mut buttons = vec![menu::Entry::new(
        menu::Command::SteamStorageHide,
        i18n::text("steam-move-hide"),
    )];
    if !moving.stopping {
        buttons.push(
            menu::Entry::new(
                menu::Command::SteamCancelMove,
                i18n::text("steam-move-stop"),
            )
            .grave()
            .holds(),
        );
    }
    buttons
}

/// The word each refusal is said with in the catalogues — see
/// `steam-refused`.
fn refusal_word(declined: &Declined) -> &'static str {
    match declined {
        Declined::DriveRoot => "drive-root",
        Declined::NotEmpty => "not-empty",
        Declined::NotWritable => "not-writable",
        Declined::NotExecutable => "not-executable",
        Declined::AlreadyALibrary => "already",
        Declined::NotListed => "not-listed",
        Declined::InUse(_) => "in-use",
        Declined::FolderThere => "folder-there",
        Declined::Shared => "shared",
        Declined::NoRoom => "no-room",
        Declined::Running => "running",
        Declined::Unmovable => "unmovable",
        Declined::AnotherMove => "another-move",
        Declined::Other(_) => "other",
    }
}

/// Why something asked of Steam's storage did not happen, as a person reads
/// it: what to do about it, and never Steam's own word for it, which is in the
/// log. `name_of` finds a game's name for the one refusal that names a game.
pub(crate) fn refusal_said(
    why: &StorageRefused,
    name_of: impl Fn(u32) -> Option<String>,
) -> String {
    match why {
        StorageRefused::Unreached(_) => i18n::text("steam-could-not-be-reached").to_string(),
        StorageRefused::Declined(Declined::InUse(Some(app_id))) => match name_of(*app_id) {
            Some(game) => crate::message!("steam-refused-in-use-by", "game" => game),
            None => crate::message!("steam-refused", "why" => "in-use"),
        },
        StorageRefused::Declined(declined) => {
            crate::message!("steam-refused", "why" => refusal_word(declined))
        }
    }
}

/// What the page calls a library it is about to make, by the path it will be
/// made at: the drive's name, as Settings > Storage gives it.
fn library_name(path: &str) -> String {
    settings::steam_library_name(path)
}

impl crate::Shell {
    /// Steam's libraries and the games in each, read off the disk again and
    /// handed to the Settings tree. `true` when the tree has to be rebuilt.
    pub(crate) fn read_steam_storage(&mut self) -> bool {
        let read = match settings::steam_integration() {
            true => read(),
            false => Read::default(),
        };
        let libraries = settings::note_steam_libraries(read.listed);
        let games = settings::note_steam_games(read.root, read.games);
        libraries || games
    }

    /// The menu over one game on a library's page — see [`stored_menu_rows`].
    pub(crate) fn stored_entry_menu(&self) -> Option<([f32; 4], Option<String>, Vec<menu::Entry>)> {
        let panel = self.panels.get(self.focused_panel)?;
        let stored = panel.cursor.current_entry(&self.lattice)?.stored()?;
        let anchor = crate::ui::launch_origin(panel.width as f32, panel.height as f32);
        let elsewhere = settings::steam_libraries().iter().any(|library| {
            library.present && !settings::same_library(&library.path, &stored.library)
        });
        let moving = self.steam.moving().map(|moving| moving.app_id);
        Some((
            anchor,
            Some(stored.name.clone()),
            stored_menu_rows(stored, elsewhere, moving),
        ))
    }

    /// A game pressed on a library's page: its menu, rather than the game.
    pub(crate) fn selected_stored(&self) -> Option<&apps::Stored> {
        self.panels
            .get(self.focused_panel)?
            .cursor
            .current_entry(&self.lattice)?
            .stored()
    }

    /// Move chosen from that menu: ask where to.
    pub(crate) fn offer_to_move(&mut self, app_id: u32) {
        if let Some(busy) = self.steam.moving().map(|moving| moving.app_id) {
            if busy == app_id {
                return self.show_the_move();
            }
            let name = self.stored_name(app_id);
            return self.say_about_storage(
                name,
                vec![crate::message!("steam-refused", "why" => "another-move")],
            );
        }
        let Some((from, game)) = settings::steam_game_stored(app_id) else {
            tracing::warn!(app_id, "a move was asked for a game no library lists");
            return;
        };
        let choice = MoveChoice::new(
            app_id,
            game.name.clone(),
            game.size,
            &from,
            &settings::steam_libraries(),
            |path| settings::steam_library_room(path).map(|room| room.free),
        );
        let (lines, buttons, start) = (choice.lines(), choice.buttons(), choice.start());
        self.move_choice = Some(choice);
        let from = self.dialog_origin();
        self.dialog
            .ask(from, Some(icons::STEAM.to_string()), lines, buttons, start);
        self.needs_redraw = true;
    }

    /// One library pressed on that panel: move the game into it, and put the
    /// move's own panel up.
    pub(crate) fn move_into(&mut self, app_id: u32, library: u8) {
        let Some(choice) = self
            .move_choice
            .take()
            .filter(|choice| choice.app_id == app_id)
        else {
            tracing::warn!(app_id, "a library was pressed with no move on the screen");
            return;
        };
        let Some(place) = choice.chosen(library) else {
            tracing::warn!(
                app_id,
                library,
                "that library is not one the game can go into"
            );
            return;
        };
        tracing::info!(app_id, to = %place.path, "moving a game into another library");
        self.steam.move_game(app_id, place.path.clone());
        self.note_the_move();
        self.show_the_move();
    }

    /// Put the move's panel up, or up again after it was hidden.
    pub(crate) fn show_the_move(&mut self) {
        let Some(moving) = self.moving_now() else {
            return;
        };
        let name = self.stored_name(moving.app_id);
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            moving_lines(&name, &moving),
            moving_buttons(&moving),
            0,
        );
        self.storage_panel = Some(Panel::Moving(moving.app_id));
        self.needs_redraw = true;
    }

    /// Stop pressed on the move's panel.
    pub(crate) fn stop_the_move(&mut self) {
        tracing::info!("the move between libraries is to stop");
        self.steam.cancel_move();
        self.note_the_move();
        self.refresh_the_move();
    }

    /// The move as the page and the panel show it.
    fn moving_now(&self) -> Option<MovingNow> {
        self.steam.moving().map(|moving| MovingNow {
            app_id: moving.app_id,
            to: moving.to.clone(),
            percent: moving
                .percent
                .map(|percent| percent.clamp(0.0, 100.0).floor() as u8),
            stopping: moving.stopping,
        })
    }

    /// Hand the page the move as it stands, and rebuild it if a row would
    /// look different.
    pub(crate) fn note_the_move(&mut self) {
        if settings::note_steam_moving(self.moving_now()) {
            self.rebuild_settings();
            self.needs_redraw = true;
        }
    }

    /// Whether the panel on screen is the move's.
    fn move_panel_is_up(&self) -> bool {
        self.dialog.is_open()
            && matches!(self.storage_panel, Some(Panel::Moving(_)))
            && self
                .dialog
                .buttons
                .entries()
                .iter()
                .any(|entry| entry.command == menu::Command::SteamStorageHide)
    }

    /// Say how far the move has got on its panel, if its panel is up.
    pub(crate) fn refresh_the_move(&mut self) {
        if !self.move_panel_is_up() {
            return;
        }
        let Some(moving) = self.moving_now() else {
            return;
        };
        let name = self.stored_name(moving.app_id);
        self.dialog.say(moving_lines(&name, &moving));
        self.dialog.refresh(moving_buttons(&moving));
        self.needs_redraw = true;
    }

    /// A move has ended, however it ended.
    ///
    /// Done, it says so where the person is looking: on the panel if it is
    /// up, in the corner if it was hidden. Stopped, the panel goes — that is
    /// what was asked for. Refused, the reason is put up either way, because a
    /// move somebody set going and walked away from and that did not happen is
    /// news they would otherwise never have.
    pub(crate) fn a_move_ended(
        &mut self,
        app_id: u32,
        to: &str,
        how: Result<Moved, StorageRefused>,
    ) {
        let watched = self.move_panel_is_up();
        if matches!(self.storage_panel, Some(Panel::Moving(id)) if id == app_id) {
            self.storage_panel = None;
        }
        self.note_the_move();
        let name = self.stored_name(app_id);
        let library = settings::steam_library_name(to);
        match how {
            Ok(Moved::Done) if watched => {
                let from = self.dialog_origin();
                self.dialog.ask(
                    from,
                    Some(icons::STEAM.to_string()),
                    vec![
                        Line::Heading(name),
                        Line::Note(crate::message!("steam-moved-note", "library" => library)),
                        Line::Rule,
                    ],
                    vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
                    0,
                );
            }
            Ok(Moved::Done) => {
                self.notifications.announce(
                    &name,
                    &crate::message!("steam-moved-note", "library" => library),
                    icons::STEAM,
                );
                self.load_notification_icons();
                self.sync_notification_panel();
            }
            Ok(Moved::Cancelled) => {
                if watched {
                    self.dialog.close();
                }
            }
            Err(why) => {
                let reason = refusal_said(&why, |app| self.stored_name_known(app));
                self.say_about_storage(
                    name,
                    vec![i18n::text("steam-move-failed").to_string(), reason],
                );
            }
        }
        self.needs_redraw = true;
    }

    /// Remove drive pressed on a library's page: ask first.
    pub(crate) fn offer_to_remove_library(&mut self, path: &str) {
        self.library_to_remove = Some(path.to_string());
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(icons::FILE_DRIVE.to_string()),
            vec![
                Line::Heading(library_name(path)),
                Line::Note(i18n::text("steam-remove-library-ask").to_string()),
                Line::Rule,
            ],
            vec![
                menu::Entry::new(menu::Command::Dismiss, i18n::text("shell-keep-it")),
                menu::Entry::new(
                    menu::Command::SteamRemoveLibraryNow,
                    i18n::text("shell-remove"),
                )
                .grave(),
            ],
            // On the answer that changes nothing, as the uninstall question
            // stands: the press that lands by accident has to be the harmless
            // one.
            0,
        );
        self.needs_redraw = true;
    }

    /// The question answered: forget the library.
    pub(crate) fn remove_library_now(&mut self) {
        let Some(path) = self.library_to_remove.take() else {
            return;
        };
        self.shelve(Shelving::Remove(path));
    }

    /// Ask Steam to add, remove or repair a library, and say so while it does.
    pub(crate) fn shelve(&mut self, job: Shelving) {
        tracing::info!(%job, "asking Steam to change its libraries");
        let name = library_name(job.path());
        let key = match &job {
            Shelving::Add(_) => "steam-library-adding",
            Shelving::Remove(_) => "steam-library-removing",
            Shelving::Repair(_) => "steam-library-repairing",
        };
        self.steam.shelve(job.clone());
        let from = self.dialog_origin();
        self.dialog.ask(
            from,
            Some(icons::FILE_DRIVE.to_string()),
            vec![
                Line::Heading(name),
                Line::Note(i18n::text(key).to_string()),
                Line::Waiting,
                Line::Rule,
            ],
            vec![menu::Entry::new(
                menu::Command::SteamStorageHide,
                i18n::text("steam-move-hide"),
            )],
            0,
        );
        self.storage_panel = Some(Panel::Shelving(job));
        self.needs_redraw = true;
    }

    /// Steam has answered about a library.
    ///
    /// Added, the cursor is taken to the new library's row where it is still
    /// on this page — the row appearing is the answer, and standing on it
    /// says which. Removed, the page closes over it. Repaired, the panel says
    /// so. Refused, the panel says why, whether it was hidden or not.
    pub(crate) fn a_library_changed(&mut self, job: Shelving, how: Result<(), StorageRefused>) {
        let watched = self.dialog.is_open()
            && self.storage_panel.as_ref() == Some(&Panel::Shelving(job.clone()));
        if matches!(&self.storage_panel, Some(Panel::Shelving(held)) if *held == job) {
            self.storage_panel = None;
        }
        // Out of a library's page before the page goes: the cursor is kept by
        // position, and a page taken out from under it would leave it standing
        // in whichever library slid into that place.
        if let (Shelving::Remove(path), Ok(())) = (&job, &how) {
            self.step_out_of_the_library(path);
        }
        if self.read_steam_storage() {
            self.rebuild_settings();
        }
        let name = library_name(job.path());
        match (&job, how) {
            (Shelving::Add(path), Ok(())) => {
                if watched {
                    self.dialog.close();
                }
                self.walk_to_the_library(path);
            }
            (Shelving::Remove(_), Ok(())) => {
                if watched {
                    self.dialog.close();
                }
            }
            (Shelving::Repair(_), Ok(())) => {
                if watched {
                    let from = self.dialog_origin();
                    self.dialog.ask(
                        from,
                        Some(icons::FILE_DRIVE.to_string()),
                        vec![
                            Line::Heading(name),
                            Line::Note(i18n::text("steam-library-repaired").to_string()),
                            Line::Rule,
                        ],
                        vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
                        0,
                    );
                }
            }
            (job, Err(why)) => {
                let what = match job {
                    Shelving::Add(_) => "steam-library-not-added",
                    Shelving::Remove(_) => "steam-library-not-removed",
                    Shelving::Repair(_) => "steam-library-not-repaired",
                };
                let reason = refusal_said(&why, |app| self.stored_name_known(app));
                self.say_about_storage(name, vec![i18n::text(what).to_string(), reason]);
            }
        }
        self.needs_redraw = true;
    }

    /// Step every display's cursor out of one library's page, to the row on
    /// the Storage page it was opened from.
    fn step_out_of_the_library(&mut self, path: &str) {
        let identity = settings::steam_library_identity(path);
        let lattice = &self.lattice;
        for panel in &mut self.panels {
            let inside = |cursor: &crate::model::Cursor| {
                cursor.opened_rows(lattice).iter().any(|row| {
                    matches!(row, apps::Entry::Folder(folder) if folder.identity.as_deref() == Some(identity.as_str()))
                })
            };
            while inside(&panel.cursor) {
                if !panel.cursor.leave() {
                    break;
                }
            }
        }
        self.needs_redraw = true;
    }

    /// Put the cursor on one library's row on the Storage page, if it is still
    /// standing on that page — never pulled there from anywhere else.
    fn walk_to_the_library(&mut self, path: &str) {
        let (id, ..) = apps::SHELL_SETTINGS;
        let Some(at) = self
            .lattice
            .categories
            .iter()
            .position(|column| column.id == id)
        else {
            return;
        };
        let wanted = settings::steam_library_identity(path);
        let Some(walk) = self
            .lattice
            .categories
            .get(at)
            .and_then(|column| path_to_identity(&column.entries, &wanted))
        else {
            return;
        };
        let lattice = &self.lattice;
        let Some(panel) = self.panels.get_mut(self.focused_panel) else {
            return;
        };
        let on_the_page = panel.cursor.current_category(lattice).is_some_and(|c| c.id == id)
            && panel.cursor.opened_rows(lattice).iter().any(|row| {
                matches!(row, apps::Entry::Folder(folder) if folder.identity.as_deref() == Some("steam-storage"))
            });
        if !on_the_page {
            return;
        }
        let Some((row, into)) = walk.split_last() else {
            return;
        };
        panel.cursor.go_to_own_column(at, lattice);
        for step in into {
            panel.cursor.point_at_row(*step, lattice);
            if !panel.cursor.enter(lattice) {
                return;
            }
        }
        panel.cursor.point_at_row(*row, lattice);
        tracing::info!(%path, "the cursor stands on the new library");
        self.needs_redraw = true;
    }

    /// A panel about something on this page that did not happen: what it was
    /// about, and why.
    fn say_about_storage(&mut self, heading: String, notes: Vec<String>) {
        let from = self.dialog_origin();
        let mut lines = vec![Line::Heading(heading)];
        lines.extend(notes.into_iter().map(Line::Note));
        lines.push(Line::Rule);
        self.dialog.ask(
            from,
            Some(icons::STEAM.to_string()),
            lines,
            vec![menu::Entry::new(menu::Command::Dismiss, "OK")],
            0,
        );
        self.needs_redraw = true;
    }

    /// A game's name, from the library the account has or the manifest on the
    /// disk, or "App 440" where neither says.
    fn stored_name(&self, app_id: u32) -> String {
        self.stored_name_known(app_id)
            .unwrap_or_else(|| crate::message!("steam-app-number", "app" => app_id))
    }

    fn stored_name_known(&self, app_id: u32) -> Option<String> {
        self.steam
            .game(app_id)
            .map(|game| game.name.clone())
            .or_else(|| settings::steam_game_stored(app_id).map(|(_, game)| game.name))
    }
}

/// The rows to step through to reach the folder known by `identity`, from the
/// top of a column — the last is the row itself.
fn path_to_identity(rows: &[apps::Entry], identity: &str) -> Option<Vec<usize>> {
    for (at, row) in rows.iter().enumerate() {
        let apps::Entry::Folder(folder) = row else {
            continue;
        };
        if folder.identity.as_deref() == Some(identity) {
            return Some(vec![at]);
        }
        if let Some(mut below) = path_to_identity(&folder.entries, identity) {
            below.insert(0, at);
            return Some(below);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn listed(path: &str, label: &str, present: bool) -> Listed {
        Listed {
            path: path.to_string(),
            label: label.to_string(),
            present,
        }
    }

    fn stored(busy: bool) -> apps::Stored {
        apps::Stored {
            app_id: 440,
            name: "Team Fortress 2".to_string(),
            note: "30 GB".to_string(),
            library: "/home/someone/.local/share/Steam".to_string(),
            busy,
            moving: None,
        }
    }

    fn commands(rows: &[menu::Entry]) -> Vec<(menu::Command, bool)> {
        rows.iter().map(|row| (row.command, row.enabled)).collect()
    }

    /// Move where there is somewhere to move it, Uninstall, and the way out —
    /// and both greyed out while Steam is working on the game.
    #[test]
    fn the_menu_offers_move_and_uninstall() {
        assert_eq!(
            commands(&stored_menu_rows(&stored(false), true, None)),
            [
                (menu::Command::SteamMoveGame(440), true),
                (menu::Command::SteamUninstall(440), true),
                (menu::Command::Dismiss, true),
            ]
        );
        assert_eq!(
            commands(&stored_menu_rows(&stored(false), false, None)),
            [
                (menu::Command::SteamUninstall(440), true),
                (menu::Command::Dismiss, true),
            ],
            "one library is nowhere to move it to"
        );
        assert_eq!(
            commands(&stored_menu_rows(&stored(true), true, None)),
            [
                (menu::Command::SteamMoveGame(440), false),
                (menu::Command::SteamUninstall(440), false),
                (menu::Command::Dismiss, true),
            ]
        );
    }

    /// The game being moved offers its move's panel instead, and cannot be
    /// taken off under it.
    #[test]
    fn a_game_being_moved_offers_its_panel() {
        assert_eq!(
            commands(&stored_menu_rows(&stored(false), true, Some(440))),
            [
                (menu::Command::SteamShowMove, true),
                (menu::Command::SteamUninstall(440), false),
                (menu::Command::Dismiss, true),
            ]
        );
        // Another game moving leaves this one's Move where it was: the press
        // says why it has to wait.
        assert_eq!(
            commands(&stored_menu_rows(&stored(false), true, Some(730)))[0],
            (menu::Command::SteamMoveGame(440), true)
        );
    }

    /// Every other library that is plugged in is a button, the one it is in
    /// and the one that is not there are not, and one too small is greyed out.
    #[test]
    fn a_move_offers_every_other_library_it_fits() {
        let libraries = [
            listed("/home/someone/.local/share/Steam", "", true),
            listed("/mnt/big/SteamLibrary", "Big", true),
            listed("/mnt/small/SteamLibrary", "Small", true),
            listed("/run/media/someone/Away/SteamLibrary", "Away", false),
        ];
        let room = |path: &str| match path {
            "/mnt/big/SteamLibrary" => Some(400 * GB),
            "/mnt/small/SteamLibrary" => Some(10 * GB),
            _ => None,
        };
        let choice = MoveChoice::new(
            440,
            "Team Fortress 2".to_string(),
            30 * GB,
            "/home/someone/.local/share/Steam/",
            &libraries,
            room,
        );
        let into = |library| menu::Command::SteamMoveInto {
            app_id: 440,
            library,
        };
        assert_eq!(
            commands(&choice.buttons()),
            [
                (into(0), true),
                (into(1), false),
                (menu::Command::Dismiss, true)
            ]
        );
        assert_eq!(choice.start(), 0);
        assert_eq!(
            choice.chosen(0).map(|place| place.path.as_str()),
            Some("/mnt/big/SteamLibrary")
        );
        assert_eq!(choice.chosen(1), None, "it does not fit there");
        assert_eq!(choice.chosen(9), None);
    }

    /// Where no library has room the cursor starts on Not now, and where the
    /// room on a drive is not known the move is offered for Steam to answer.
    #[test]
    fn nowhere_with_room_starts_on_not_now() {
        let libraries = [
            listed("/home/someone/.local/share/Steam", "", true),
            listed("/home/someone/Games", "Games", true),
        ];
        let choice = |room: Option<u64>| {
            MoveChoice::new(
                440,
                "Team Fortress 2".to_string(),
                30 * GB,
                "/home/someone/.local/share/Steam",
                &libraries,
                |_| room,
            )
        };
        let nowhere = choice(Some(GB));
        assert_eq!(nowhere.start(), 1, "Not now");
        assert!(nowhere.chosen(0).is_none());
        assert!(choice(None).chosen(0).is_some(), "not known is offered");
    }

    /// Every refusal has a sentence of its own, and none of them is Steam's
    /// word: a person reads what to do, not what went wrong.
    #[test]
    fn every_refusal_is_said_in_words() {
        let refusals = [
            Declined::DriveRoot,
            Declined::NotEmpty,
            Declined::NotWritable,
            Declined::NotExecutable,
            Declined::AlreadyALibrary,
            Declined::NotListed,
            Declined::InUse(None),
            Declined::FolderThere,
            Declined::Shared,
            Declined::NoRoom,
            Declined::Running,
            Declined::Unmovable,
            Declined::AnotherMove,
            Declined::Other("NoSuchWord".to_string()),
        ];
        let mut said: Vec<String> = refusals
            .iter()
            .map(|declined| refusal_said(&StorageRefused::Declined(declined.clone()), |_| None))
            .collect();
        for sentence in &said {
            assert!(!sentence.contains("NoSuchWord"), "{sentence}");
            assert!(!sentence.trim().is_empty());
        }
        said.sort();
        said.dedup();
        assert_eq!(said.len(), refusals.len(), "each says something different");

        let unreached = refusal_said(
            &StorageRefused::Unreached("the socket closed (os error 104)".to_string()),
            |_| None,
        );
        assert!(!unreached.contains("os error"), "{unreached}");

        let named = refusal_said(
            &StorageRefused::Declined(Declined::InUse(Some(440))),
            |_| Some("Team Fortress 2".to_string()),
        );
        assert!(named.contains("Team Fortress 2"), "{named}");
    }

    /// The panel keeps its height from the first word to the last: the lights
    /// until Steam says, then the bar, then the lights again while it stops.
    #[test]
    fn the_move_panel_keeps_its_shape() {
        let moving = |percent, stopping| MovingNow {
            app_id: 440,
            to: "/mnt/big/SteamLibrary".to_string(),
            percent,
            stopping,
        };
        let starting = moving_lines("Team Fortress 2", &moving(None, false));
        let going = moving_lines("Team Fortress 2", &moving(Some(42), false));
        let stopping = moving_lines("Team Fortress 2", &moving(Some(42), true));
        assert_eq!(starting.len(), going.len());
        assert_eq!(going.len(), stopping.len());
        assert!(starting.contains(&Line::Waiting));
        assert!(going.contains(&Line::Progress(42)));
        assert!(stopping.contains(&Line::Waiting));

        let buttons = |stopping| -> Vec<menu::Command> {
            moving_buttons(&moving(Some(42), stopping))
                .iter()
                .map(|button| button.command)
                .collect()
        };
        assert_eq!(
            buttons(false),
            [
                menu::Command::SteamStorageHide,
                menu::Command::SteamCancelMove
            ]
        );
        assert_eq!(buttons(true), [menu::Command::SteamStorageHide]);
    }

    /// The walk to a library's row finds it under the page it is on.
    #[test]
    fn the_walk_finds_a_library_by_its_path() {
        let page = |identity: &str, entries| {
            apps::Entry::Folder(apps::Folder {
                title_message: None,
                comment_message: None,
                identity: Some(identity.to_string()),
                title: identity.to_string(),
                comment: None,
                icon: None,
                entries,
                place: None,
                chosen: false,
                over_the_list: false,
                person: None,
                portrait: None,
                used: None,
            })
        };
        let column = vec![
            page("shell-appearance", Vec::new()),
            page(
                "shell-games",
                vec![page(
                    "Steam",
                    vec![
                        page("shell-integration", Vec::new()),
                        page(
                            "steam-storage",
                            vec![
                                page(&settings::steam_library_identity("/home/s"), Vec::new()),
                                page(&settings::steam_library_identity("/mnt/x"), Vec::new()),
                            ],
                        ),
                    ],
                )],
            ),
        ];
        assert_eq!(
            path_to_identity(&column, &settings::steam_library_identity("/mnt/x/")),
            Some(vec![1, 0, 1, 1])
        );
        assert_eq!(path_to_identity(&column, "nowhere"), None);
    }
}
