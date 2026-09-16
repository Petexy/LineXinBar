//! The rows the user has ticked, when they are picking several at once.
//!
//! Every other command in this shell is about the row under the cursor: the
//! menu is raised over it, the panel names it, and the thing that happens
//! happens to it. That is the right shape for nearly everything — a game is
//! started, a setting is set, a file is opened — and it is the wrong shape for
//! the one job a file manager exists to do, which is moving a *handful* of
//! things somewhere. Doing it a row at a time is the same six presses over and
//! over, and the user is the one keeping count.
//!
//! So there is a second way to be selected. The Select multiple row of the
//! explorer's own menu turns the column into one that is being *marked*: the
//! press that opens a file ticks it instead, the menu raised while it is on is
//! about the whole set rather than about one row, and the head row at the top
//! of the column says how many there are and is the way back out.
//!
//! ## Why the ticks are not on the rows
//!
//! They could have been — [`crate::apps::Folder`] already carries a `chosen`
//! flag, and a `marked` beside it would have drawn itself. They are here
//! instead, for the reason the *cursor* is not in the tree either: the tree is
//! what is on the disk, and where somebody is standing in it is not. A mark is
//! the same kind of fact. It belongs to what the user is doing, it survives a
//! column being read again from a filesystem that has never heard of it, and
//! it would otherwise have had to be threaded through every one of the thirty-
//! five places in this shell that builds a row which can never carry one.
//!
//! What the drawing gets instead is this, beside the cursor, and it answers one
//! question per row: [`Marks::holds`].
//!
//! ## What is kept
//!
//! The path, and enough of the row to act on it without looking again. A
//! transfer holds its subject for exactly this reason — see
//! [`crate::transfer::Source`] — and the argument is the same one twice over
//! here: the column can be read again while the marking is on, another display
//! is driving its own cursor, and a set that read itself off whatever happened
//! to be in the tree when Delete was pressed would be a set that could act on
//! the wrong files.
//!
//! Keyed by path, so a column read again finds the same rows, and ordered by it
//! so that whatever is done to the set is done in an order that does not depend
//! on when each row was ticked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::apps::Entry;

/// One ticked row, held as what acting on it will need.
///
/// Two kinds because there are two columns this can happen in and what can be
/// done in them has nothing in common: the things in a listing are carried,
/// renamed and thrown away, and the things in the trash are put back or
/// destroyed. A single kind carrying both would be a set whose every use began
/// by asking which sort of thing it had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// A file or a folder in a listing, held as a transfer would carry it —
    /// which is what every act on one of these needs, Delete included.
    OnDisk(crate::transfer::Source),
    /// One thing in the trash, held as the trash describes it.
    Trashed(crate::trash::Trashed),
}

impl Picked {
    /// What the row under the cursor would be, if it is one that can be ticked
    /// at all.
    ///
    /// `None` for everything else in the tree, which is what leaves the head
    /// rows pressable while a column is being marked: the field at the top of a
    /// folder is not a thing anybody is copying, and neither is the row that
    /// ends the marking.
    pub fn of(entry: &Entry) -> Option<Self> {
        match entry {
            Entry::File(file) => Some(Self::OnDisk(crate::transfer::Source {
                path: file.path.clone(),
                name: file.name.clone(),
                note: file.note.clone(),
                glyph: file.glyph,
                folder: false,
            })),
            Entry::Folder(folder) => {
                let Some(crate::files::Place::Directory(at, _)) = &folder.place else {
                    return None;
                };
                Some(Self::OnDisk(crate::transfer::Source {
                    path: at.clone(),
                    name: folder.title.clone(),
                    // What the row said about it, which for a folder already
                    // opened is what was found in it and otherwise is when it
                    // was last written — the same line the picker shows.
                    note: folder.comment.clone().unwrap_or_default(),
                    glyph: crate::icons::FILE_FOLDER,
                    folder: true,
                }))
            }
            Entry::Trashed(item) => Some(Self::Trashed(item.clone())),
            _ => None,
        }
    }

    /// Where it is on the disk now — which for something in the trash is the
    /// name the trash gave it, not the name the row wears.
    pub fn path(&self) -> &Path {
        match self {
            Self::OnDisk(source) => &source.path,
            Self::Trashed(item) => &item.at,
        }
    }

    /// What the user calls it.
    pub fn name(&self) -> &str {
        match self {
            Self::OnDisk(source) => &source.name,
            Self::Trashed(item) => &item.name,
        }
    }

    /// Whether it leads somewhere, which is what decides the wording of every
    /// question asked about a set.
    pub fn folder(&self) -> bool {
        match self {
            Self::OnDisk(source) => source.folder,
            Self::Trashed(item) => item.folder,
        }
    }

    /// It as a transfer would carry it, where it is something a transfer can
    /// carry at all.
    pub fn on_disk(&self) -> Option<&crate::transfer::Source> {
        match self {
            Self::OnDisk(source) => Some(source),
            Self::Trashed(_) => None,
        }
    }

    /// And it as the trash describes it, where it is in the trash.
    pub fn trashed(&self) -> Option<&crate::trash::Trashed> {
        match self {
            Self::Trashed(item) => Some(item),
            Self::OnDisk(_) => None,
        }
    }
}

/// The rows ticked in one column, and which column that is.
#[derive(Debug, Clone)]
pub struct Marks {
    /// The column the ticks belong to. Leaving it ends the marking — see
    /// `Shell::keep_the_marking_honest`, which is where that is enforced, and
    /// why: a set gathered in one folder and acted on in another would be a
    /// Delete whose subject the user can no longer see.
    column: crate::files::Place,
    picked: BTreeMap<PathBuf, Picked>,
}

impl Marks {
    /// Start marking `column`, with nothing ticked yet.
    pub fn new(column: crate::files::Place) -> Self {
        Self {
            column,
            picked: BTreeMap::new(),
        }
    }

    /// Which column these belong to.
    pub fn column(&self) -> &crate::files::Place {
        &self.column
    }

    /// Whether this is the trash, which is what decides both what the menu
    /// offers and what kind of thing is in the set.
    pub fn in_the_trash(&self) -> bool {
        self.column == crate::files::Place::Trash
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    /// Whether this row is one of the ticked ones.
    ///
    /// Asked once per row the drawing puts on screen, which is a dozen or so —
    /// the columns behind hold rows too, and a path that is not in the set
    /// answers in a comparison or two of a sorted map. A folder where somebody
    /// has ticked everything is the worst case and it is still a lookup, not a
    /// walk.
    pub fn holds(&self, entry: &Entry) -> bool {
        marked_path(entry).is_some_and(|path| self.picked.contains_key(path))
    }

    /// Tick it, or take the tick off. Answers with whether it is ticked now.
    pub fn toggle(&mut self, picked: Picked) -> bool {
        let path = picked.path().to_path_buf();
        if self.picked.remove(&path).is_some() {
            return false;
        }
        self.picked.insert(path, picked);
        true
    }

    /// Tick every row of `entries` that can be ticked, leaving alone the ones
    /// that cannot. Answers with how many are ticked now.
    ///
    /// The head rows are what it walks past: the field at the top of a folder
    /// and the row that ends the marking are not things anybody is copying, so
    /// Select all means every *file and folder* rather than every row.
    pub fn all_of(&mut self, entries: &[Entry]) -> usize {
        for entry in entries {
            if let Some(picked) = Picked::of(entry) {
                self.picked.insert(picked.path().to_path_buf(), picked);
            }
        }
        self.len()
    }

    /// Take every tick off, without ending the marking.
    pub fn clear(&mut self) {
        self.picked.clear();
    }

    /// The ones a transfer could carry, in path order — every one of them,
    /// where the column is a listing, and none at all in the trash.
    pub fn sources(&self) -> Vec<crate::transfer::Source> {
        self.picked
            .values()
            .filter_map(Picked::on_disk)
            .cloned()
            .collect()
    }

    /// And the ones the trash knows about, on the same terms.
    pub fn trashed(&self) -> Vec<crate::trash::Trashed> {
        self.picked
            .values()
            .filter_map(Picked::trashed)
            .cloned()
            .collect()
    }

    /// What the set is called in a question about it — see [`how_many`].
    pub fn named(&self) -> String {
        let only = self.picked.values().next().map_or("", Picked::name);
        how_many(self.len(), only)
    }

    /// And what it is made of, where that is worth saying — see
    /// [`what_they_are`].
    pub fn made_of(&self) -> String {
        let folders = self.picked.values().filter(|one| one.folder()).count();
        what_they_are(self.len() - folders, folders)
    }

    /// Whether anything ticked leads somewhere, which is what decides whether a
    /// question about the set has to say that the contents go too.
    pub fn any_folder(&self) -> bool {
        self.picked.values().any(Picked::folder)
    }

    /// What the head row of the column says under itself.
    pub fn note(&self) -> String {
        match self.len() {
            0 => crate::i18n::text("shell-nothing-selected").to_string(),
            count => crate::message!("count-selected", "count" => count),
        }
    }
}

/// Where on the disk the row under `entry` is, for the three kinds of row that
/// can be ticked.
///
/// Deliberately narrower than `row_is_at`, which answers the same question
/// about a shelved song and one of somebody's own games as well: those live in
/// columns that cannot be marked, and a set that could hold one would be a set
/// with no folder to carry it out of.
fn marked_path(entry: &Entry) -> Option<&Path> {
    match entry {
        Entry::File(file) => Some(&file.path),
        Entry::Folder(folder) => match &folder.place {
            Some(crate::files::Place::Directory(at, _)) => Some(at),
            _ => None,
        },
        Entry::Trashed(item) => Some(&item.at),
        _ => None,
    }
}

/// How a set of things is named in a question about it, given how many there
/// are and what the one of them is called.
///
/// "3 things" and not "3 files": a set can hold folders, and a panel that
/// called a folder a file at the moment it asked whether to destroy it would be
/// the shell being least accurate where it matters most. The one case that is
/// named exactly is a set of one — which happens the moment somebody unticks
/// their way down to it, and is also every act made without any marking at all
/// — because a question about one thing should say its name, exactly as every
/// other question in this shell does.
///
/// Here rather than in [`crate::transfer`] or beside each panel, and taking
/// counts rather than rows, because five different things are named this way:
/// the row left on screen while a set is carried, the panel that says a carry
/// is running, the Delete question, and the two the trash asks. One rule, one
/// place, whichever kind of row the set is made of.
pub fn how_many(count: usize, only: &str) -> String {
    match count {
        1 => only.to_string(),
        count => crate::message!("count-things", "count" => count),
    }
}

/// The line under that: what the set is made of, where saying so is any use.
///
/// Empty for a set of one, whose row already says what it is, and empty for a
/// set that is all of one kind — "3 things" over "3 files" is the same sentence
/// twice. It is the mixed set this exists for, because that is the one where
/// the count on its own hides that a folder is going too.
pub fn what_they_are(files: usize, folders: usize) -> String {
    if files + folders < 2 || files == 0 || folders == 0 {
        return String::new();
    }
    crate::message!("count-files-and-folders", "files" => files, "folders" => folders)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::Folder;
    use crate::files::{Item, Place, Shows};

    fn file(name: &str) -> Entry {
        Entry::File(Item {
            name: name.to_string(),
            note: "1.2 kB".to_string(),
            path: PathBuf::from("/home/somebody/Documents").join(name),
            mime: "text/plain",
            glyph: crate::icons::FILE_PAGE,
        })
    }

    fn folder(name: &str) -> Entry {
        Entry::Folder(Folder {
            title_message: None,
            comment_message: None,
            identity: None,
            title: name.to_string(),
            comment: Some("3 files".to_string()),
            icon: Some(crate::icons::FILE_FOLDER.to_string()),
            entries: Vec::new(),
            place: Some(Place::Directory(
                PathBuf::from("/home/somebody/Documents").join(name),
                Shows::Everything,
            )),
            chosen: false,
            over_the_list: false,
            person: None,
            portrait: None,
        })
    }

    /// The field at the head of a folder, which is the row a marking has to
    /// walk past: it is not a thing anybody is copying.
    fn head() -> Entry {
        let mut rows = Vec::new();
        crate::apps::head(&mut rows, crate::apps::Searched::Folder, "", 0, 3);
        rows.remove(0)
    }

    fn documents() -> Marks {
        Marks::new(Place::Directory(
            PathBuf::from("/home/somebody/Documents"),
            Shows::Everything,
        ))
    }

    /// A tick goes on, and the same press takes it off again. The set is the
    /// whole of what a marking is, so this is the whole of what a press does.
    #[test]
    fn a_press_ticks_a_row_and_the_next_one_unticks_it() {
        let mut marks = documents();
        assert!(marks.is_empty());
        let notes = Picked::of(&file("notes.txt")).unwrap();
        assert!(marks.toggle(notes.clone()), "the first press ticks it");
        assert_eq!(marks.len(), 1);
        assert!(marks.holds(&file("notes.txt")));
        assert!(!marks.toggle(notes), "and the second takes the tick off");
        assert!(marks.is_empty());
        assert!(!marks.holds(&file("notes.txt")));
    }

    /// A row read again off the disk is the same row: the ticks are held by
    /// path, so a column narrowed by the field while it is being marked comes
    /// back still ticked.
    #[test]
    fn a_tick_survives_the_column_being_read_again() {
        let mut marks = documents();
        marks.toggle(Picked::of(&file("notes.txt")).unwrap());
        // A second reading of the same directory: different `Entry`, same file.
        let fresh = file("notes.txt");
        assert!(marks.holds(&fresh));
    }

    /// Select all takes every row that is a thing and leaves alone every row
    /// that is not — which in a folder is the field at the head of it.
    #[test]
    fn selecting_them_all_walks_past_the_rows_that_are_not_things() {
        let rows = vec![head(), folder("Invoices"), file("a.txt"), file("b.txt")];
        let mut marks = documents();
        assert_eq!(marks.all_of(&rows), 3);
        assert!(!marks.holds(&rows[0]), "the field is not a thing to copy");
        assert!(rows[1..].iter().all(|row| marks.holds(row)));
        marks.clear();
        assert!(marks.is_empty(), "and Clear takes every one of them off");
    }

    /// What the panels call a set: the one thing's own name, or a count that
    /// never says "files" about something that might be a folder.
    #[test]
    fn a_set_is_named_by_its_one_row_or_by_how_many_there_are() {
        let mut marks = documents();
        marks.toggle(Picked::of(&file("notes.txt")).unwrap());
        assert_eq!(marks.named(), "notes.txt");
        assert_eq!(marks.note(), "1 selected");
        assert_eq!(
            marks.made_of(),
            "",
            "one row says what it is on the row itself"
        );

        marks.toggle(Picked::of(&file("tax.pdf")).unwrap());
        assert_eq!(marks.named(), "2 things");
        assert_eq!(marks.note(), "2 selected");
        assert_eq!(
            marks.made_of(),
            "",
            "and a set that is all of one kind would be saying it twice"
        );

        marks.toggle(Picked::of(&folder("Invoices")).unwrap());
        assert_eq!(marks.named(), "3 things");
        assert_eq!(
            marks.made_of(),
            "2 files and 1 folder",
            "a mixed set is the one that has to say a folder is going"
        );
        assert!(marks.any_folder());
    }

    /// Nothing ticked is a state somebody reaches by unticking their way back
    /// down, and the panel has to have something to say about it.
    #[test]
    fn an_empty_set_still_answers() {
        let marks = documents();
        assert_eq!(marks.note(), "Nothing selected");
        assert_eq!(marks.named(), "0 things");
        assert!(!marks.any_folder());
        assert!(marks.sources().is_empty());
    }

    /// A folder is carried as a transfer would carry it, mark and all, so the
    /// row left standing on the picker is the row that was under the cursor.
    #[test]
    fn a_folder_is_picked_up_as_the_thing_a_transfer_carries() {
        let picked = Picked::of(&folder("Invoices")).unwrap();
        assert!(picked.folder());
        assert_eq!(picked.name(), "Invoices");
        assert_eq!(
            picked.path(),
            Path::new("/home/somebody/Documents/Invoices")
        );
        let source = picked.on_disk().expect("a folder in a listing is carried");
        assert_eq!(source.glyph, crate::icons::FILE_FOLDER);
        assert_eq!(source.note, "3 files");
        assert!(picked.trashed().is_none());
    }
}
