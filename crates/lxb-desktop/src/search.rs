//! Looking for something anywhere on the bar at once.
//!
//! Every column on the bar already answers "what is in here", and the long ones
//! carry a field of their own that narrows them. What none of them can answer
//! is "where is it" — which column a program was filed under, which page of
//! Settings a switch is on, which of three stores a game came from. That is the
//! question this answers: somebody on the start screen who starts typing is
//! asking for a thing by name, and the name is all they are expected to know.
//!
//! What is looked through is everything the bar shows except the user's own
//! files — applications, the three stores and every game in them, the trophy
//! shelves, and every page of Settings however deep. The files are left out on
//! purpose: the Music, Video and Images shelves and the explorer's folders are
//! as big as somebody's home directory, and each already carries a search of
//! its own that is better at them than a list mixed in with programs could be.
//! Their rows — the shelf, the Files row — are found like any other page. So,
//! on the same argument, is every other list that carries a field of its own:
//! the keyboard arrangements are found as the page they are on.
//!
//! What comes back is a column of the bar's own rows, copied, with the line
//! under each rewritten to say where it lives. A copy rather than a reference
//! because a column on this bar is a slice of rows, and what a search finds is
//! scattered all over the tree; and because a copy can say where it came from
//! without the original having to. See [`Home`] for what a press on one does.

use std::collections::HashSet;

use crate::apps::{Category, Entry};

/// The id the search's own column goes by.
///
/// Never on the bar. The column lives in [`crate::model::Lattice::searches`],
/// one per display that is searching, and a cursor that is searching stands in
/// it instead of in the category it began from — see
/// [`crate::model::Cursor::begin_search`].
pub const COLUMN: &str = "search";

/// Between the column a result lives in and the page inside it: "Settings ›
/// Display". The shell's own face has it, which is the only thing asked of it.
const INSIDE: &str = " › ";

/// Between where a result lives and what it says about itself: "Games ·
/// Comprehensive Kerbal Archive Network Client".
const AND: &str = " · ";

/// What one display's search has found.
#[derive(Debug, Clone)]
pub struct Found {
    /// Which search this is — the number the cursor standing in it holds.
    pub id: u64,
    /// The rows, as the column the cursor stands in.
    pub column: Category,
    /// Where each of those rows lives, in the same order.
    pub homes: Vec<Home>,
    /// What each of them is, in terms a rebuilt column can be searched for:
    /// the row a cursor was standing on is found again by this, so the list
    /// rebuilding under somebody reading it does not move them to another row.
    keys: Vec<String>,
}

impl Found {
    /// A search that has found nothing yet.
    pub fn empty(id: u64) -> Found {
        Found {
            id,
            column: column(Vec::new()),
            homes: Vec::new(),
            keys: Vec::new(),
        }
    }

    /// What the row at `row` is, as [`Self::row_of`] would find it again.
    pub fn key(&self, row: usize) -> Option<&str> {
        self.keys.get(row).map(String::as_str)
    }

    /// Which row is the one [`Self::key`] said, if it is still here.
    pub fn row_of(&self, key: &str) -> Option<usize> {
        self.keys.iter().position(|kept| kept == key)
    }
}

/// The search's own column, holding `entries`.
fn column(entries: Vec<Entry>) -> Category {
    Category {
        id: COLUMN,
        title: "Search",
        icon: crate::icons::SEARCH,
        entries,
    }
}

/// Where one thing a search found lives on the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    /// The category it is in, by the id the bar knows it by.
    pub column: &'static str,
    /// The rows walked from the top of that category's column to reach it,
    /// the last of them the row itself.
    pub path: Vec<Step>,
    /// What a press on it does.
    pub press: Press,
}

/// One row of the walk to a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Where the row was when the search looked, if the search looked at the
    /// column itself. `None` for a row of a library the search read whole
    /// rather than off the bar: the column on the bar is sorted however the
    /// user asked and may be narrowed by its own field, so where the row stood
    /// in the library says nothing about where it stands there.
    pub row: Option<usize>,
    /// What it was called, which is how the walk makes sure the number still
    /// points at the same row — and how it finds the row when it does not.
    pub title: String,
}

impl Step {
    /// Which row of `entries` this step is, now.
    ///
    /// The row the search saw, where it still carries the same name; failing
    /// that, the first row of the list itself that does — a column rebuilt
    /// since the search looked, or one the search never looked at, has moved
    /// its rows about, and the name is what the user asked for.
    ///
    /// Never a row standing over the list — the index at the head of a
    /// library carries the names of the games it leads to — because the
    /// search never walked one.
    pub fn row_in(&self, entries: &[Entry]) -> Option<usize> {
        let is = |entry: &Entry| !entry.over_the_list() && entry.title() == self.title;
        self.row
            .filter(|&row| entries.get(row).is_some_and(is))
            .or_else(|| entries.iter().position(is))
    }
}

/// What pressing a result does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Started from the results, exactly as its own column would start it: an
    /// application, a game, one of somebody's own ROMs. What was asked for is
    /// the thing itself, and taking the user across the bar first would be a
    /// press spent on a journey.
    Here,
    /// A place — a page of Settings, a store, a shelf, the disk. The search
    /// is put away and the bar spreads back out on the column it lives in, with
    /// it opened: a page read out of its own column is a page the user can
    /// step back out of and find the rest of that column around it.
    There,
}

/// The libraries the shell holds whole, whose columns on the bar may be
/// narrowed by a field of their own.
///
/// Read here rather than off the bar for exactly that reason: somebody who
/// searched their Steam library for one game an hour ago has a column holding
/// one game, and a search of everything that looked there would find a single
/// title out of three hundred.
#[derive(Debug, Default)]
pub struct Libraries {
    /// Every game of the Steam account, as the column's own rows.
    pub steam: Vec<Entry>,
    /// Every game of the Epic account, likewise.
    pub epic: Vec<Entry>,
    /// Every game in the Trophies column, before its index and its own field.
    pub trophies: Vec<Entry>,
}

/// Look for `query` everywhere on the bar.
///
/// Ranked by how the name meets what was typed — the whole name, then its
/// start, then the start of one of its words, then anywhere in it, and last an
/// application whose own keywords hold it — and within each by where on the
/// bar the row stands, left to right and top to bottom, which is the order the
/// user would have walked to it in.
///
/// Nothing at all for a query of nothing: a field emptied by backspacing is a
/// field asking nothing yet, not one asking for everything.
pub fn find(id: u64, categories: &[Category], libraries: &Libraries, query: &str) -> Found {
    let Some(needle) = lxb_steam::library::sought(query) else {
        return Found::empty(id);
    };
    let mut hunt = Hunt {
        needle,
        seen: HashSet::new(),
        found: Vec::new(),
    };
    for category in categories {
        let named = category.display_title();
        let library = match category.id {
            id if id == crate::apps::steam_column() => Some(&libraries.steam),
            id if id == crate::apps::epic_column() => Some(&libraries.epic),
            crate::trophies::COLUMN => Some(&libraries.trophies),
            _ => None,
        };
        match library {
            Some(rows) => hunt.library(category.id, named, rows),
            None => hunt.walk(
                category.id,
                &mut vec![named.to_string()],
                &mut Vec::new(),
                &category.entries,
            ),
        }
    }
    // Stable, so rows that meet the query equally stay in the order the bar
    // has them in.
    hunt.found.sort_by_key(|candidate| candidate.rank);
    let mut entries = Vec::with_capacity(hunt.found.len());
    let mut homes = Vec::with_capacity(hunt.found.len());
    let mut keys = Vec::with_capacity(hunt.found.len());
    for candidate in hunt.found {
        entries.push(candidate.entry);
        homes.push(candidate.home);
        keys.push(candidate.key);
    }
    Found {
        id,
        column: column(entries),
        homes,
        keys,
    }
}

/// One search, as it walks the bar.
struct Hunt {
    /// What was typed, folded the way every name is before it is compared.
    needle: String,
    /// What has been found already, by [`key`]: an application is in one
    /// column only, but a game is a row of its store and a row of the index
    /// at the head of it.
    seen: HashSet<String>,
    found: Vec<Candidate>,
}

struct Candidate {
    rank: u8,
    entry: Entry,
    home: Home,
    key: String,
}

impl Hunt {
    /// Walk one column of the bar, and every page inside it that is part of
    /// the bar rather than part of the disk.
    ///
    /// `names` is where the walk has got to, as the user reads it — the
    /// category's name and every page opened since — and `trail` the same walk
    /// as rows.
    fn walk(
        &mut self,
        column: &'static str,
        names: &mut Vec<String>,
        trail: &mut Vec<Step>,
        entries: &[Entry],
    ) {
        // A column that carries a field of its own is a list long enough to
        // need one — the keyboard arrangements are six hundred of them, under
        // every country on earth — and it is searched by that field. Listed
        // here, "port" found Portugal ahead of Portal, and "w" Western Sahara.
        // The page holding the list is found like any other.
        if entries.first().is_some_and(|head| head.search().is_some()) {
            return;
        }
        for (row, entry) in entries.iter().enumerate() {
            // What stands over a list is about the list: its field, the row
            // that empties it, the index of a library, the answer row of a
            // picker. None of them is somewhere to go, and the index is the
            // same games again.
            if entry.over_the_list() {
                continue;
            }
            let step = Step {
                row: Some(row),
                title: entry.title().to_string(),
            };
            if let Some(press) = press_for(entry) {
                trail.push(step.clone());
                self.consider(column, names, trail, entry, press);
                trail.pop();
            }
            if descends(entry) {
                let Some(inside) = entry.entries() else {
                    continue;
                };
                names.push(step.title.clone());
                trail.push(step);
                self.walk(column, names, trail, inside);
                trail.pop();
                names.pop();
            }
        }
    }

    /// One library read whole: its rows are the games, and nothing inside
    /// them is.
    fn library(&mut self, column: &'static str, named: &str, rows: &[Entry]) {
        let names = vec![named.to_string()];
        for entry in rows {
            let Some(press) = press_for(entry) else {
                continue;
            };
            let trail = [Step {
                row: None,
                title: entry.title().to_string(),
            }];
            self.consider(column, &names, &trail, entry, press);
        }
    }

    /// Keep `entry` if it answers to what was typed, and has not been kept
    /// already.
    fn consider(
        &mut self,
        column: &'static str,
        names: &[String],
        trail: &[Step],
        entry: &Entry,
        press: Press,
    ) {
        let Some(rank) = rank(entry, &self.needle) else {
            return;
        };
        let key = key(column, trail, entry);
        if !self.seen.insert(key.clone()) {
            return;
        }
        let mut copy = entry.bare();
        let place = names.join(INSIDE);
        copy.say_instead(
            match entry.comment().filter(|said| !said.trim().is_empty()) {
                Some(said) => format!("{place}{AND}{said}"),
                None => place,
            },
        );
        self.found.push(Candidate {
            rank,
            entry: copy,
            home: Home {
                column,
                path: trail.to_vec(),
                press,
            },
            key,
        });
    }
}

/// What a press on this row does from the results, or `None` for a row the
/// search does not offer at all.
///
/// Left out: a value of a setting, which is one answer to a question the page
/// asks and means nothing without the page around it — "On", "Large", "Dark"
/// — and a bar, which is a value too; and every row that is a file, which is
/// what the shelves and the explorer are for.
fn press_for(entry: &Entry) -> Option<Press> {
    match entry {
        Entry::App(_) | Entry::Game(_) | Entry::EpicGame(_) | Entry::Rom(_) | Entry::Ps3Game(_) => {
            Some(Press::Here)
        }
        Entry::Folder(_)
        | Entry::Steam(_)
        | Entry::RetroArch(_)
        | Entry::Epic(_)
        | Entry::Ps3(_)
        | Entry::Facts(_)
        | Entry::Typed(_)
        | Entry::Partition(_) => Some(Press::There),
        // A game on the Trophies shelf, which opens onto what has been won in
        // it. Its achievements are not offered: the search is for somewhere
        // to go, and an achievement is a line in a list.
        Entry::Trophy(row) => matches!(
            row.key,
            crate::trophies::Key::SteamGame(_)
                | crate::trophies::Key::RetroGame(..)
                | crate::trophies::Key::EpicGame(_)
                | crate::trophies::Key::Ps3Game(_)
        )
        .then_some(Press::There),
        Entry::Media(_)
        | Entry::File(_)
        | Entry::Choice(_)
        | Entry::Bar(_)
        | Entry::Search(_)
        | Entry::Pick(_)
        | Entry::Make(_)
        | Entry::Sweep(_)
        | Entry::Done(_)
        | Entry::Trashed(_)
        | Entry::Stored(_) => None,
    }
}

/// Whether the walk goes on inside this row.
///
/// Into every page of the bar's own, and into a console's shelf of games, and
/// not into anything that is a place on the disk: a folder, a shelf of the
/// user's files, the disks themselves. Those are found as rows — the Files
/// row, the Music shelf — and what is in them is not this search's to list.
fn descends(entry: &Entry) -> bool {
    match entry {
        Entry::Folder(folder) => folder.place.is_none() && crate::apps::shelf_of(entry).is_none(),
        _ => false,
    }
}

/// How well a row's name meets what was typed, best first; `None` where it
/// does not meet it at all.
fn rank(entry: &Entry, needle: &str) -> Option<u8> {
    let title = lxb_steam::library::sort_key(entry.title());
    if title == needle {
        return Some(0);
    }
    if title.starts_with(needle) {
        return Some(1);
    }
    if starts_a_word(&title, needle) {
        return Some(2);
    }
    if title.contains(needle) {
        return Some(3);
    }
    // An application's keywords are words its author chose for exactly this:
    // "browser" for a program called Firefox. Last, because they are the
    // author's words and not the ones on the screen.
    let keywords = entry
        .app()
        .map(|app| app.keywords.as_slice())
        .unwrap_or_default();
    keywords
        .iter()
        .any(|word| lxb_steam::library::sort_key(word).contains(needle))
        .then_some(4)
}

/// Whether `needle` begins one of the words of `title` other than the first.
fn starts_a_word(title: &str, needle: &str) -> bool {
    title.match_indices(needle).any(|(at, _)| {
        title[..at]
            .chars()
            .next_back()
            .is_some_and(|before| !before.is_alphanumeric())
    })
}

/// What a result is, in terms that survive the column it came from being
/// rebuilt: the game or the program where the row is one, and otherwise where
/// it is.
fn key(column: &str, trail: &[Step], entry: &Entry) -> String {
    match entry {
        Entry::App(app) => format!("app:{}", app.path.display()),
        Entry::Game(game) => format!("steam:{}", game.app_id),
        Entry::EpicGame(game) => format!("epic:{}", game.app_name),
        Entry::Rom(rom) => format!("rom:{}", rom.path.display()),
        Entry::Ps3Game(game) => format!("ps3:{}", game.id),
        _ => {
            let walk: Vec<&str> = trail.iter().map(|step| step.title.as_str()).collect();
            format!("{column}:{}", walk.join("/"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::apps::{App, Folder, Game};

    fn app(name: &str) -> Entry {
        Entry::App(App {
            name: name.into(),
            comment: Some("does a thing".into()),
            icon: None,
            exec: "true".into(),
            terminal: false,
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            path: PathBuf::from(format!("/usr/share/applications/{name}.desktop")),
            wm_class: None,
        })
    }

    fn page(title: &str, entries: Vec<Entry>) -> Folder {
        Folder {
            title_message: None,
            comment_message: None,
            identity: None,
            title: title.into(),
            comment: None,
            icon: None,
            entries,
            place: None,
            chosen: false,
            over_the_list: false,
            person: None,
            portrait: None,
            used: None,
        }
    }

    fn folder(title: &str, entries: Vec<Entry>) -> Entry {
        Entry::Folder(page(title, entries))
    }

    fn game(app_id: u32, name: &str) -> Entry {
        Entry::Game(Game {
            ways: 1,
            progress: None,
            app_id,
            name: name.into(),
            note: "Installed".into(),
            installed: true,
            updating: false,
            steam_client: true,
            standing: lxb_steam::library::Standing::Ready,
            stuck: false,
            waiting_for_steam: false,
        })
    }

    fn category(id: &'static str, entries: Vec<Entry>) -> Category {
        Category {
            id,
            title: "Column",
            icon: "icon",
            entries,
        }
    }

    fn titles(found: &Found) -> Vec<&str> {
        found.column.entries.iter().map(Entry::title).collect()
    }

    #[test]
    fn an_empty_query_finds_nothing_rather_than_everything() {
        let bar = [category("games", vec![app("Firefox")])];
        let found = find(1, &bar, &Libraries::default(), "   ");
        assert!(found.column.entries.is_empty());
        assert_eq!(found.column.id, COLUMN);
    }

    #[test]
    fn the_best_meeting_of_the_name_comes_first_and_the_bar_breaks_ties() {
        let bar = [
            category(
                "internet",
                vec![app("Web Steamer"), app("Upsteam"), app("Steamworks")],
            ),
            category("games", vec![app("Steam"), app("Steam Link")]),
        ];
        let found = find(1, &bar, &Libraries::default(), "STEAM ");
        assert_eq!(
            titles(&found),
            [
                "Steam",
                "Steamworks",
                "Steam Link",
                "Web Steamer",
                "Upsteam"
            ],
            "whole name, then its start, then a word's start, then anywhere"
        );
    }

    #[test]
    fn a_keyword_finds_a_program_its_name_does_not() {
        let Entry::App(mut firefox) = app("Firefox") else {
            unreachable!()
        };
        firefox.keywords = vec!["Browser".into(), "Web".into()];
        let bar = [category(
            "internet",
            vec![app("Browser Tools"), Entry::App(firefox)],
        )];
        let found = find(1, &bar, &Libraries::default(), "browser");
        assert_eq!(titles(&found), ["Browser Tools", "Firefox"]);
    }

    #[test]
    fn pages_are_found_however_deep_and_say_where_they_are() {
        let settings = category(
            "settings",
            vec![folder(
                "Display",
                vec![folder("Night light", vec![app("unused")])],
            )],
        );
        let found = find(1, &[settings], &Libraries::default(), "night");
        assert_eq!(titles(&found), ["Night light"]);
        let home = &found.homes[0];
        assert_eq!(home.column, "settings");
        assert_eq!(home.press, Press::There);
        assert_eq!(
            home.path,
            [
                Step {
                    row: Some(0),
                    title: "Display".into()
                },
                Step {
                    row: Some(0),
                    title: "Night light".into()
                }
            ]
        );
        assert_eq!(
            found.column.entries[0].comment(),
            Some("Settings › Display"),
            "a page says which page it is on"
        );
        assert!(
            found.column.entries[0]
                .entries()
                .is_some_and(<[Entry]>::is_empty),
            "the copy carries none of the tree under it"
        );
    }

    #[test]
    fn a_program_is_started_here_and_says_its_column_before_its_own_line() {
        let bar = [category("games", vec![app("CKAN")])];
        let found = find(1, &bar, &Libraries::default(), "ckan");
        assert_eq!(found.homes[0].press, Press::Here);
        assert_eq!(
            found.column.entries[0].comment(),
            Some("Games · does a thing")
        );
    }

    #[test]
    fn values_files_and_the_rows_over_a_list_are_not_offered() {
        let choice = Entry::Choice(crate::apps::Choice {
            title: "Steam value".into(),
            comment: None,
            icon: None,
            swatch: None,
            material: None,
            chosen: false,
            acts: false,
            setting: None,
            over_the_list: false,
        });
        let index = Entry::Folder(Folder {
            over_the_list: true,
            ..page("Steam index", vec![app("Steam inside the index")])
        });
        let bar = [category("settings", vec![choice, index, app("Steam")])];
        let found = find(1, &bar, &Libraries::default(), "steam");
        assert_eq!(titles(&found), ["Steam"]);
    }

    #[test]
    fn a_place_on_the_disk_is_found_but_not_walked_into() {
        let files = Entry::Folder(Folder {
            place: Some(crate::files::Place::Volumes(
                crate::files::Shows::Everything,
            )),
            ..page("Files", vec![app("Files inside the disk")])
        });
        let bar = [category("system", vec![files])];
        let found = find(1, &bar, &Libraries::default(), "files");
        assert_eq!(titles(&found), ["Files"]);
        assert_eq!(found.homes[0].press, Press::There);
    }

    #[test]
    fn a_list_with_a_field_of_its_own_is_left_to_that_field() {
        let mut countries = Vec::new();
        crate::apps::head(&mut countries, crate::apps::Searched::Layouts, "", 1, 1);
        countries.push(folder("Portugal", Vec::new()));
        let bar = [
            category("settings", vec![folder("Keyboard layout", countries)]),
            category(crate::apps::steam_column(), Vec::new()),
        ];
        let libraries = Libraries {
            steam: vec![game(10, "Portal")],
            ..Libraries::default()
        };
        assert_eq!(titles(&find(1, &bar, &libraries, "port")), ["Portal"]);
        assert_eq!(
            titles(&find(1, &bar, &libraries, "keyboard")),
            ["Keyboard layout"],
            "the page holding the list is found"
        );
    }

    #[test]
    fn a_library_is_read_whole_and_not_off_its_narrowed_column() {
        // The column on the bar holds one game, narrowed by its own field; the
        // library holds both.
        let narrowed = category(crate::apps::steam_column(), vec![game(10, "Portal")]);
        let libraries = Libraries {
            steam: vec![game(10, "Portal"), game(20, "Portal 2")],
            ..Libraries::default()
        };
        let found = find(1, &[narrowed], &libraries, "portal");
        assert_eq!(titles(&found), ["Portal", "Portal 2"]);
        assert_eq!(found.homes[1].path[0].row, None);
        assert_eq!(found.homes[1].press, Press::Here);
    }

    #[test]
    fn the_same_game_twice_is_found_once() {
        let libraries = Libraries {
            steam: vec![game(10, "Portal"), game(10, "Portal")],
            ..Libraries::default()
        };
        let bar = [category(crate::apps::steam_column(), Vec::new())];
        let found = find(1, &bar, &libraries, "portal");
        assert_eq!(titles(&found), ["Portal"]);
    }

    #[test]
    fn a_step_finds_its_row_by_name_once_the_number_has_moved() {
        let step = Step {
            row: Some(0),
            title: "Night light".into(),
        };
        assert_eq!(step.row_in(&[folder("Night light", Vec::new())]), Some(0));
        let moved = [
            folder("Resolution", Vec::new()),
            folder("Night light", Vec::new()),
        ];
        assert_eq!(step.row_in(&moved), Some(1));
        let index = Entry::Folder(Folder {
            over_the_list: true,
            ..page("Night light", Vec::new())
        });
        assert_eq!(
            step.row_in(&[index, folder("Night light", Vec::new())]),
            Some(1),
            "not the index of a library standing over the list"
        );
        assert_eq!(step.row_in(&[folder("Gone", Vec::new())]), None);
    }

    #[test]
    fn a_row_is_found_again_by_what_it_is() {
        let bar = [category("games", vec![app("Alpha"), app("Alphabet")])];
        let before = find(1, &bar, &Libraries::default(), "alpha");
        let key = before.key(1).expect("a second row").to_string();
        let bar = [category("games", vec![app("Alphabet"), app("Alpha")])];
        let after = find(1, &bar, &Libraries::default(), "alpha");
        assert_eq!(
            after.column.entries[after.row_of(&key).unwrap()].title(),
            "Alphabet"
        );
    }
}
