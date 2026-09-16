//! The disk itself, walked as a column of the bar.
//!
//! Multimedia's and Graphics' shelves answer "what have I got" — one walk of
//! the home directory, gathered by kind, everything of that kind in one list
//! however deep it was buried. This module answers the other question, the one
//! a shelf cannot: "what is *in there*". It is the disk as it actually is,
//! folder by folder, and the only thing it gathers is the one directory the
//! user is standing in.
//!
//! ## Why it is a column and not a window
//!
//! A file manager is a window with two panes, a tree on one side and a list on
//! the other, and the tree is what says where you are. This bar already has a
//! tree: stepping right into a subcategory opens a column of its own beside the
//! one it came from, and the columns behind keep the row they were opened from.
//! That trail *is* a path — `Root → usr → lib` reads left to right across the
//! screen with the category above it — so a folder is a subcategory, a file is
//! a row, and nothing had to be invented to say where the user is. Left is up a
//! level because Left is how every column is left.
//!
//! So there is no explorer here in the sense of a program: there is a rule for
//! turning a directory into rows, and the bar does the rest.
//!
//! ## Where it starts
//!
//! Three kinds of place, and they are the three answers to "which disk":
//!
//! * **Home** — the user's own things, which is where somebody is looking
//!   nine times in ten.
//! * **Root** — the machine, from `/`. Everything else on the system is under
//!   it, including the mounted drives, but reaching a stick through
//!   `/run/media/<user>/` is four presses of somebody who already knows the
//!   convention.
//! * **Every drive that is mounted** — which is why the third row exists. See
//!   [`drives`] for what counts as one.
//!
//! The list is built when the user steps into Files rather than when the shell
//! starts, because a drive plugged in during a session is the whole point of
//! having the row.
//!
//! ## What it costs
//!
//! One `readdir` and one `lstat` per entry, on the press that opens the folder,
//! on the thread that draws. Deliberately not the worker the shelves are walked
//! on: a shelf is the whole home directory and takes seconds, where an ordinary
//! folder is under a millisecond and the largest directory on a stock machine —
//! `/usr/lib`, six thousand entries — measures 16 ms warm. That is one frame,
//! once, on a press somebody made. What a worker would buy instead is a column
//! that opens empty and fills in afterwards, which is the one thing the shelves
//! went to a thread to avoid.
//!
//! The listing is capped at [`MOST`] rows and says so when the cap bites. A
//! directory with a million files in it is a directory nobody is going to
//! scroll to the end of, and building a million rows for it would stall the
//! frame the press landed on for as long as anybody would notice.
//!
//! Nothing is cached. A folder is read again every time it is stepped into, so
//! what is on the bar is what is on the disk as of the press — and the rows of
//! the folders *beside* the one being opened are dropped as it opens, so the
//! tree holds the path the user is standing in and not the whole of where they
//! have been. See [`crate::model::Cursor::open_place`].

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::apps::{Entry, Folder};

/// The most rows one directory is listed as.
///
/// Ten thousand is past anything a person scrolls through and short of what
/// costs a frame to build: the rows are the listing's only real expense, and
/// this bounds it at about a megabyte and a couple of milliseconds whatever is
/// in the folder. What is left out is said on the row above — see [`note`].
const MOST: usize = 10_000;

/// What the row that opens all this says for itself.
///
/// Here rather than where the row is built, because opening it writes the note
/// back — one path for every place, and this is what the Files row's own note
/// is when the place it stands for is the list of disks.
pub const WHAT_FILES_ARE: &str = "Your folder, this machine, and anything plugged in";

/// What a row of the explorer stands for on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// The Files row itself: the disks there are to look in, worked out when
    /// it is opened rather than when the shell started.
    Volumes(Shows),
    /// One directory, listed when it is stepped into.
    Directory(PathBuf, Shows),
    /// The trash: everything the user has deleted, wherever on this machine it
    /// went.
    ///
    /// Not a [`Place::Directory`] pointed at `~/.local/share/Trash/files`,
    /// although that is where most of what it lists is sitting. Three things
    /// are wrong with that reading and each of them on its own settles it: the
    /// names in there are the trash's rather than the user's, half of what
    /// belongs in the column is in a `.Trash-1000` at the top of some other
    /// disk, and the rows are things to put back or destroy rather than things
    /// to open. It is a listing of a *specification*, not of a folder.
    ///
    /// It carries no [`Shows`]. Every walk that is choosing something is
    /// choosing a file to keep — a wallpaper, a cover, somebody's face — and
    /// the trash is where things go that the user has said they do not want.
    /// A picker that offered it would be offering an answer that disappears
    /// the next time the trash is emptied.
    Trash,
}

impl Place {
    /// What of a directory is listed, wherever this place is on the disk.
    pub fn shows(&self) -> Shows {
        match self {
            Self::Volumes(shows) | Self::Directory(_, shows) => *shows,
            // The disk as it is, which is the only walk that reaches it — see
            // [`Place::Trash`], which is offered from nowhere else.
            Self::Trash => Shows::Everything,
        }
    }
}

/// How a folder is read: the order its rows come back in, and whether the
/// names beginning with a dot are among them.
///
/// One value rather than two arguments, because the two are one thing — a pair
/// of preferences the user holds about *reading a listing*, chosen from the
/// same menu, written into the same settings file, applied to every folder
/// alike. Deliberately not on [`Place`], which is where [`Shows`] travels:
/// `Shows` is a fact about the walk and belongs to the column, and these two
/// are facts about the person and belong to the shell, which holds them once
/// and hands them down on every read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct How {
    /// Which of the nine orders the rows come back in.
    pub sort: crate::media::Sort,
    /// Whether names beginning with a dot are listed.
    ///
    /// Off by default — see [`crate::settings::show_hidden`], which is where
    /// the answer is kept and why it is off.
    pub hidden: bool,
}

impl How {
    /// A to Z with the dotfiles left out: what an unasked shell reads a folder
    /// with, and what every walk that is *choosing* something reads it with —
    /// see [`crate::transfer`], whose columns are a path rather than a list.
    pub fn plain() -> Self {
        Self {
            sort: crate::media::Sort::NameAscending,
            hidden: false,
        }
    }
}

/// How much of a folder a column of it lists.
///
/// The disk is walked for two different reasons now, and the second one is not
/// browsing: choosing the picture that stands behind the whole shell, under
/// Settings > Appearance > Theme > Wallpaper. A column opened for that is
/// answering a question rather than showing what is there, and everything the
/// answer cannot be is noise in it — a wallpaper column that listed somebody's
/// tax return would be offering it.
///
/// It travels on [`Place`] rather than on the row, because it is a fact about
/// the walk and not about the folder: every column reached from a wallpaper
/// picker is one, all the way down, and it is the place a step opens that
/// carries it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shows {
    /// The disk as it is — the file explorer under Files.
    #[default]
    Everything,
    /// Only what could stand behind a screen: the folders to keep walking
    /// through, the pictures, and the films. See [`worth_showing`].
    Scenery,
    /// Only folders, because a folder is the answer: the walk somebody makes to
    /// say where their games are.
    ///
    /// The third reason the disk is walked, and the first one where what is
    /// being chosen is the *column* rather than a row in it. So this one
    /// changes two things rather than one: the files are left out, since not
    /// one of them can be the answer, and every column carries a row at its
    /// head that says "this folder" — see [`crate::apps::Pick`], and
    /// [`crate::apps::place_rows`], which is where that row is put on.
    ///
    /// It carries what the answer is *for*, because the walk is the only thing
    /// that knows: a press on that head row is several columns away from the
    /// row the picker was opened from, and by then that row has been rebuilt
    /// underneath it more than once.
    Folders(crate::settings::Picking),
    /// Only what could be one of somebody's own game's pictures: the folders to
    /// keep walking through, and the pictures themselves.
    ///
    /// The same walk [`Shows::Scenery`] is and the same rows, with the films
    /// left out — what stands behind a game is a still, and a row offering a
    /// film would be an answer this shell cannot use. It carries which of the
    /// two pictures is being chosen, because a walk several folders deep is the
    /// only thing left that knows: the row it started from is a column away and
    /// has been rebuilt since. See [`crate::retroarch::Piece`].
    Picture(crate::retroarch::Piece),
    /// Only what could be somebody's face: the folders to keep walking through,
    /// and the pictures.
    ///
    /// The same walk [`Shows::Picture`] is, and it carries nothing — unlike that
    /// one, which has to say *which* of a game's two pictures is being chosen.
    /// There is only ever one account form open at a time, and it is the form
    /// that knows whose face this is; see [`crate::users::set_picture`].
    Portrait,
}

impl Shows {
    /// What a column of this walk is being chosen for, if it is a choice at
    /// all.
    pub fn picking(self) -> Option<crate::settings::Picking> {
        match self {
            Shows::Folders(picking) => Some(picking),
            Shows::Everything | Shows::Scenery | Shows::Picture(_) | Shows::Portrait => None,
        }
    }

    /// Which of a game's two pictures this walk is choosing, if it is choosing
    /// one at all.
    pub fn piece(self) -> Option<crate::retroarch::Piece> {
        match self {
            Shows::Picture(piece) => Some(piece),
            Shows::Everything | Shows::Scenery | Shows::Folders(_) | Shows::Portrait => None,
        }
    }
}

/// Whether this entry belongs in a column listed as `shows`.
///
/// Folders always: a picture is somewhere further down, and a picker that hid
/// the folders would be one nobody could walk. Beyond that it is the two kinds
/// of file that can be drawn — which is [`crate::media::Kind`]'s own table, so
/// that a column offering to make a wallpaper of a file and the thumbnailer
/// asked to draw one cannot disagree about what a picture is.
fn worth_showing(shows: Shows, path: &Path, leads_to_a_folder: bool) -> bool {
    match shows {
        Shows::Everything => true,
        Shows::Scenery => {
            leads_to_a_folder
                || matches!(
                    crate::media::kind_of(path),
                    Some(crate::media::Kind::Image | crate::media::Kind::Video)
                )
        }
        // Not even to be walked past. A folder chooser that listed the files as
        // well would be offering rows that cannot be pressed, in a column whose
        // every other row can be — and the folder somebody is looking for would
        // be somewhere in the middle of their music.
        Shows::Folders(_) => leads_to_a_folder,
        // And a picture, which is the one kind a game's cover or the picture
        // behind it can be. A film is left out where the wallpaper takes one:
        // what stands behind a game is a still, and a cover is a card an inch
        // high.
        // And a picture, which is the one kind a face can be — for the reason a
        // game's cover can only be one: a row offering a film as somebody's
        // portrait would be an answer this shell cannot use.
        Shows::Picture(_) | Shows::Portrait => {
            leads_to_a_folder || crate::media::kind_of(path) == Some(crate::media::Kind::Image)
        }
    }
}

/// One file, as a row of a directory's column.
///
/// Deliberately not a [`crate::media::File`], though the two describe the same
/// object from a distance. A shelved file is a thing the user *has* — its title
/// is the name without the extension, because nobody thinks of a song as
/// `Yesterday.flac`, and the line under it is the folder it was found in,
/// because that is the only thing telling two copies of it apart. Neither is
/// true here: in a folder the extension is half of what the user came to see,
/// and the folder is the column they are standing in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The file name, extension and all.
    pub name: String,
    /// How big it is and when it was last written — see [`facts`].
    pub note: String,
    pub path: PathBuf,
    /// What it would be handed to an application as. `application/octet-stream`
    /// for anything the table below does not know, which is the honest answer:
    /// it means "some bytes", nothing claims to open it, and Open falls through
    /// to whatever the desktop's own `xdg-open` makes of it.
    pub mime: &'static str,
    /// The mark it is drawn with. A file of one of the three kinds the shelves
    /// hold gets that shelf's mark — a song is a song whichever column it is
    /// being looked at from — and everything else gets the page.
    pub glyph: &'static str,
}

/// What one look at a place came back with.
pub struct Shown {
    /// The rows for the column, the field at the head of them included.
    pub rows: Vec<Entry>,
    /// What the row this column hangs on now says: what is in the folder,
    /// whether or not a search is narrowing what is being shown of it.
    pub note: String,
    /// Which of the nine orders this listing could actually be put in, for the
    /// Sort panel to grey the rest — see [`crate::media::Sort::orders`].
    pub orders: crate::media::Orders,
}

/// The rows the Files subcategory holds: the user's own folder, the machine,
/// and whatever is mounted.
///
/// Built on the press rather than kept, so a drive plugged in half an hour into
/// a session is on the bar the moment somebody goes looking for it.
///
/// Not sorted, and deliberately: these three are in the order somebody reaches
/// for them, and "the machine" does not belong under D for disk. They are
/// searchable all the same, because the field is at the head of every column
/// this module builds and a machine can have a dozen shares mounted on it.
pub fn volumes(query: &str, shows: Shows) -> Shown {
    let mut places = Vec::new();
    if let Some(home) = home() {
        places.push(place(
            "Home",
            &home,
            crate::icons::FILE_HOME,
            Some(crate::screenshot::abbreviated(&home)),
            shows,
        ));
    }
    places.push(place(
        "Root",
        Path::new("/"),
        crate::icons::FILE_DRIVE,
        None,
        shows,
    ));
    for drive in drives(&read_mounts()) {
        places.push(place(
            &drive.title,
            &drive.at,
            crate::icons::FILE_DRIVE,
            Some(drive.at.display().to_string()),
            shows,
        ));
    }

    // And the trash, under all of them, because it is not a disk: the three
    // rows above are places on this machine and this one is a place in the
    // *specification* — everything the user has deleted, gathered out of the
    // home trash and out of every mounted volume's own. Last for that reason
    // rather than by ranking, and only where the disk is being browsed: a walk
    // choosing a wallpaper or somebody's face must not be offered a file that
    // disappears the next time the trash is emptied.
    if shows == Shows::Everything {
        places.push(trash_row());
    }

    let found = places.len();
    let kept: Vec<Entry> = places
        .into_iter()
        .filter(|row| matched(row.title(), query))
        .collect();
    // No New folder row: the three rows above are the disks there are to look
    // in, and the list of disks is not a folder anything can be made in.
    let mut rows = crate::apps::place_rows(kept, query, found, at_the_head(None, shows), None);
    // One walk has an answer that is not a file on this disk: an account's
    // avatar can be *none*, and that belongs in the column asking which picture
    // rather than beside the row that opened it. It stands over the list, so the
    // column still opens on the first disk. See [`crate::settings::
    // no_avatar_row`], which is where the row and its wording live — the tree
    // writes its own rows, and this only says where one goes.
    if shows == Shows::Portrait {
        if let Some(row) = crate::settings::no_avatar_row() {
            rows.insert(0, row);
        }
    }
    Shown {
        note: WHAT_FILES_ARE.to_string(),
        rows,
        orders: crate::media::Orders::default(),
    }
}

/// The row that opens the trash.
///
/// The count is read here rather than left to the press, because a Trash row
/// with nothing behind it is a row that cannot be stepped into at all — an
/// empty column is the one shape this bar cannot show — and a row that did
/// nothing when pressed and never said why would be the shell appearing to be
/// broken. So the line under it says "Empty", which is the answer to what the
/// press would have shown.
///
/// It is one `readdir` per trash directory that exists, which on a stock
/// machine is one, on the press that opens Files. See [`crate::trash::bins`].
fn trash_row() -> Entry {
    let items = crate::trash::listing().len();
    Entry::Folder(Folder {
        title: "Trash".to_string(),
        comment: Some(match items {
            0 => "Empty".to_string(),
            1 => "1 item".to_string(),
            items => format!("{items} items"),
        }),
        // The bin, which is the mark the rest of the shell already wears for
        // taking something off this machine — see [`crate::icons::UNINSTALL`].
        // This is the literal case: it is a bin, and it is where everything
        // that row destroys would have gone if it had been one of the user's
        // own files.
        icon: Some(crate::icons::UNINSTALL.to_string()),
        entries: Vec::new(),
        place: Some(Place::Trash),
        chosen: false,
        over_the_list: false,
        person: None,
        portrait: None,
    })
}

/// Everything in the trash, as a column: the row that empties it, and then
/// what is in there.
///
/// The head row is what makes the column enterable when the trash holds one
/// thing, and it is deliberately absent when the trash holds nothing — an
/// Empty trash row over an empty trash is a control with nothing to act on,
/// and the row that opened the column has already said "Empty".
///
/// There is no search field, which is the one way this column is less than a
/// folder's. It is deliberate rather than an omission, and it is not a strong
/// argument: a trash can hold hundreds of things. What is on the other side of
/// it is that the rows are not what a field would be narrowing — a name here is
/// what the file was called before it was deleted, and what it is called on the
/// disk is something else — so a field would be searching one set of names over
/// a column listed by another. If it turns out to be wanted, it goes in the same
/// way every other one does; see [`crate::apps::head`].
///
/// The order is the explorer's, whatever the user has set it to, on the facts
/// the trash has: see [`trash_order`], which is where the deletion date takes
/// the place of a created time that would only ever say when the file was
/// thrown away.
pub fn trash(sort: crate::media::Sort) -> Shown {
    trash_of(crate::trash::listing(), sort)
}

/// The same, told what is in the trash.
///
/// Split off so the shape of the column and the nine orders can be exercised
/// against a list made for the purpose, rather than against the trash of
/// whoever is running the tests — which would be reading somebody's deleted
/// files to find out whether a sort works.
fn trash_of(mut listing: Vec<crate::trash::Trashed>, sort: crate::media::Sort) -> Shown {
    let (mut folders, mut files) = (0usize, 0usize);
    for item in &listing {
        if item.folder {
            folders += 1;
        } else {
            files += 1;
        }
    }
    // The nine orders, applied to what the trash knows, on the same facts a
    // folder's own listing is ordered by. Always, including the order the
    // trash is already read in: what it arrives in is a property of
    // [`crate::trash::listing`], and a column that was only in the right order
    // because of what its caller happened to do would be one that quietly
    // stopped being so the day that changed.
    listing.sort_by(|a, b| trash_order(sort, a, b));
    let orders = crate::media::Orders {
        // A trashed thing has no creation date the shell can honestly report:
        // what `stat` gives is when it was renamed into `files/`, which is
        // when it was *deleted*. That is a real fact and it is what the two
        // Newest/Oldest rows order by here, said plainly on the row.
        created: !listing.is_empty(),
        modified: listing.iter().any(|item| item.modified.is_some()),
    };

    let mut rows: Vec<Entry> = Vec::with_capacity(listing.len() + 1);
    if !listing.is_empty() {
        rows.push(Entry::Sweep(crate::apps::Sweep::new(listing.len())));
    }
    rows.extend(listing.into_iter().map(Entry::Trashed));
    Shown {
        note: note((folders, files), 0),
        rows,
        orders,
    }
}

/// Put one trashed thing before another, in `sort`'s order.
///
/// The folders-first rule the explorer's own listing keeps is deliberately not
/// kept here. In a folder that rule is about the shape of the thing — the rows
/// that lead somewhere stand above the rows that are somewhere — and in the
/// trash no row leads anywhere: they are all things that were deleted, and
/// what somebody is looking for is the one they deleted by mistake, whichever
/// kind it was.
fn trash_order(
    sort: crate::media::Sort,
    a: &crate::trash::Trashed,
    b: &crate::trash::Trashed,
) -> std::cmp::Ordering {
    use crate::media::Sort;
    let by_name = |a: &crate::trash::Trashed, b: &crate::trash::Trashed| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.at.cmp(&b.at))
    };
    let then = || by_name(a, b);
    match sort {
        Sort::NameAscending => then(),
        Sort::NameDescending => by_name(b, a),
        Sort::LargestFirst => b.size.cmp(&a.size).then_with(then),
        Sort::SmallestFirst => a.size.cmp(&b.size).then_with(then),
        Sort::Type => described(&a.from)
            .0
            .cmp(described(&b.from).0)
            .then_with(then),
        // The deletion date, which is the one date this column has that a
        // folder's listing does not — and the only honest reading of "newest"
        // for a thing whose every timestamp on disk is the moment it was
        // thrown away.
        Sort::NewestFirst => b.deleted_at.cmp(&a.deleted_at).then_with(then),
        Sort::OldestFirst => a.deleted_at.cmp(&b.deleted_at).then_with(then),
        Sort::LastChangedFirst => {
            crate::media::by_time(a.modified, b.modified, true).then_with(then)
        }
        Sort::LongestUntouchedFirst => {
            crate::media::by_time(a.modified, b.modified, false).then_with(then)
        }
    }
}

/// One of those rows: a way in to a disk, with how much room is left on it
/// under the name.
///
/// The room rather than the path, where there is a filesystem to ask: "18.2 GiB
/// free of 119.4 GiB" is the question somebody actually has about a drive, and
/// a row that could not be asked says where it is instead.
fn place(
    title: &str,
    at: &Path,
    glyph: &'static str,
    fallback: Option<String>,
    shows: Shows,
) -> Entry {
    let note = room(at).or(fallback);
    Entry::Folder(Folder {
        title: title.to_string(),
        comment: note,
        icon: Some(glyph.to_string()),
        entries: Vec::new(),
        place: Some(Place::Directory(at.to_path_buf(), shows)),
        chosen: false,
        over_the_list: false,
        person: None,
        portrait: None,
    })
}

/// Everything in `at` whose name holds `query`, as rows: the folders first and
/// then the files, each group in `sort`'s order.
///
/// Folders above files whatever the order, because that is the shape of the
/// thing — the rows that lead somewhere stand above the rows that *are*
/// somewhere, exactly as the shelves stand above the applications in the
/// columns they hang in. It also keeps the two orders that a directory cannot
/// answer for honest: a folder has no size worth reporting — what `stat` gives
/// is the size of the index, not of what is in it — and no type at all, so in
/// those orders the folders stay alphabetical among themselves rather than
/// being ranked by a number that means nothing.
///
/// Dotfiles are left out unless `how` says otherwise. They are configuration
/// rather than possessions — nobody's photographs are in `~/.cache` — and a
/// home directory listed with them in it opens on forty rows of program state
/// before the first thing the user recognises. The shell's own settings live in
/// the Settings column, which is where somebody looking for them is already
/// going.
///
/// Which is an argument about what a listing *opens* with and not about what
/// the user may ever see, so there is a way past it: [`How::hidden`], set from
/// the Show hidden files row of the explorer's own menu. It is one answer for
/// every folder and it is written down, exactly as the order is — see
/// [`crate::settings::show_hidden`].
///
/// An unreadable directory comes back empty, and the note above says so rather
/// than the column pretending the folder had nothing in it.
pub fn listing(at: &Path, query: &str, how: How, shows: Shows) -> Shown {
    let Ok(reading) = std::fs::read_dir(at) else {
        return Shown {
            rows: Vec::new(),
            note: "This cannot be opened".to_string(),
            orders: crate::media::Orders::default(),
        };
    };

    let mut found: Vec<Found> = Vec::new();
    let mut folders = 0usize;
    let mut files = 0usize;
    let mut left_out = 0usize;
    let mut orders = crate::media::Orders::default();

    for entry in reading.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            // A name that is not UTF-8 is a name this shell can list and never
            // hand to anything: an `Exec` line is a string. A row that opened
            // nothing would be worse than no row.
            left_out += 1;
            continue;
        };
        if name.starts_with('.') && !how.hidden {
            continue;
        }
        if folders + files >= MOST {
            left_out += 1;
            continue;
        }

        let path = entry.path();
        // What the entry *is*, following links: `/lib` is a symbolic link to
        // `usr/lib` on any merged-usr system, and a Root column that listed it
        // as a file would be a Root column nobody could walk down. The link's
        // own facts are of no interest here — what the user pressed is where it
        // points.
        let leads_to_a_folder = match entry.file_type() {
            Ok(kind) if kind.is_symlink() => std::fs::metadata(&path)
                .map(|facts| facts.is_dir())
                .unwrap_or(false),
            Ok(kind) => kind.is_dir(),
            Err(_) => false,
        };
        // Before it is counted, because the count is what the row above the
        // column says and that row is about the column somebody is looking at.
        // A picker saying "3 folders, 412 files" over a listing of four
        // photographs would be describing a different folder.
        if !worth_showing(shows, &path, leads_to_a_folder) {
            continue;
        }
        if leads_to_a_folder {
            folders += 1;
        } else {
            files += 1;
        }

        // Counted before it is narrowed, and read only if it survives: the row
        // above says what is in the folder whether or not a search is on, and
        // the `stat` behind a size and a date is the only thing here that costs
        // anything per entry. A query typed into a folder of six thousand files
        // therefore costs one `readdir` and a handful of `stat`s, not six
        // thousand of them per letter.
        if !matched(name, query) {
            continue;
        }

        let facts = std::fs::symlink_metadata(&path).ok();
        let size = facts.as_ref().map(|facts| facts.len()).unwrap_or_default();
        let created = facts.as_ref().and_then(|facts| facts.created().ok());
        let modified = facts.as_ref().and_then(|facts| facts.modified().ok());
        orders.created |= created.is_some();
        orders.modified |= modified.is_some();

        let entry = if leads_to_a_folder {
            Entry::Folder(Folder {
                title: name.to_string(),
                // What is *in* it is not asked: that is one `readdir` per row,
                // and a folder of a thousand folders would read a thousand
                // directories to draw one column. The date is already in the
                // `stat` this row cost anyway, and it is replaced by what was
                // found the moment somebody steps in.
                comment: modified.and_then(date),
                icon: Some(crate::icons::FILE_FOLDER.to_string()),
                entries: Vec::new(),
                place: Some(Place::Directory(path.clone(), shows)),
                chosen: false,
                over_the_list: false,
                person: None,
                portrait: None,
            })
        } else {
            let (mime, glyph) = described(&path);
            Entry::File(Item {
                name: name.to_string(),
                note: facts_note(size, modified),
                path: path.clone(),
                mime,
                glyph,
            })
        };
        found.push(Found {
            entry,
            folder: leads_to_a_folder,
            order: name.to_lowercase(),
            path,
            size,
            created,
            modified,
        });
    }

    found.sort_by(|a, b| order_by(how.sort, a, b));
    Shown {
        note: note((folders, files), left_out),
        rows: crate::apps::place_rows(
            found.into_iter().map(|found| found.entry).collect(),
            query,
            // What the field is narrowing, which is everything in the folder
            // that could have been listed — the rows left out by [`MOST`]
            // included, since a search that says "3 of 200" over a folder of
            // ten thousand would be counting the wrong thing.
            folders + files + left_out,
            at_the_head(Some(at), shows),
            can_make_a_folder(at, shows).then(|| crate::apps::Make {
                at: at.to_path_buf(),
            }),
        ),
        orders,
    }
}

/// Whether a column of `at` carries the row that makes a new folder in it.
///
/// Two conditions, and they are about two different things.
///
/// The walk has to be the disk being *browsed*. Every other walk is choosing
/// something — a wallpaper, a game's cover, somebody's face, the folder their
/// ROMs are in — and a brand new empty folder is not the answer to any of those
/// questions. The one that comes closest is [`Shows::Folders`], and it is the
/// clearest case of all: that column already carries an answer at its head, and
/// two head rows where one says "this folder" and the other makes a different
/// one is a column asking somebody to read carefully.
///
/// And the folder has to be one this user can write to. `/usr/lib` is four
/// presses from Root and everything in it belongs to the machine; a head row
/// offering to make a folder there would be an offer nobody on this system can
/// take, standing over six thousand rows, on every column of the way down.
/// This is the one guess about permissions the explorer makes in advance, and
/// it is made because the row is *furniture* rather than a command somebody
/// went looking for — the argument [`crate::main::media_rows`] gives for not
/// greying Copy and Move is an argument about a menu the user opened on
/// purpose.
///
/// `access(2)` and not a `stat` of the mode: the mode is three bits and the
/// answer is those bits, plus the user's groups, plus any ACL on the directory,
/// plus whether the filesystem is mounted read-only. The kernel knows all four
/// and nothing here does.
fn can_make_a_folder(at: &Path, shows: Shows) -> bool {
    if shows != Shows::Everything {
        return false;
    }
    let Ok(path) = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string for the length of the
    // call, and `access` reads nothing else.
    unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// The row that stands over one of these columns, where the walk is a choice.
///
/// `at` is the folder the column is of, or `None` for the column of volumes —
/// which gets no such row, because the list of disks is not a folder anybody
/// can put their games in. Stepping into one of them is what reaches a folder,
/// and that column carries the row.
fn at_the_head(at: Option<&Path>, shows: Shows) -> Option<crate::apps::Pick> {
    let about = shows.picking()?;
    Some(crate::apps::Pick {
        at: at?.to_path_buf(),
        about,
        comment: about.note().to_string(),
    })
}

/// One entry of a listing, with what an order needs to place it.
struct Found {
    entry: Entry,
    /// Whether it leads somewhere. Folders come first in every order.
    folder: bool,
    /// The name folded to lower case, which is what every order falls back to.
    order: String,
    /// The path, which settles the order between two entries of the same name —
    /// impossible in one directory, and the tie-break costs nothing to keep.
    path: PathBuf,
    size: u64,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
}

/// Put `a` before `b`, or after it, in `sort`'s order.
///
/// The shelves' nine orders, applied to a directory. Folders first whatever the
/// answer, and within a group the name settles anything the order does not:
/// two files of the same size, or written in the same second, come out in the
/// order the user already knows.
fn order_by(sort: crate::media::Sort, a: &Found, b: &Found) -> std::cmp::Ordering {
    use crate::media::Sort;
    if a.folder != b.folder {
        // Folders first: `false` sorts before `true`, and a folder is `true`.
        return b.folder.cmp(&a.folder);
    }
    let by_name = |a: &Found, b: &Found| a.order.cmp(&b.order).then_with(|| a.path.cmp(&b.path));
    let then = || by_name(a, b);
    // A directory has no size and no type. Ranking folders by the size of the
    // index the filesystem keeps for them would be sorting by a number that has
    // nothing to do with what is inside, so those two orders leave the folders
    // where the user can find them.
    if a.folder && matches!(sort, Sort::LargestFirst | Sort::SmallestFirst | Sort::Type) {
        return then();
    }
    match sort {
        Sort::NameAscending => then(),
        Sort::NameDescending => by_name(b, a),
        Sort::LargestFirst => b.size.cmp(&a.size).then_with(then),
        Sort::SmallestFirst => a.size.cmp(&b.size).then_with(then),
        Sort::Type => kind_of_row(&a.entry)
            .cmp(kind_of_row(&b.entry))
            .then_with(then),
        Sort::NewestFirst => crate::media::by_time(a.created, b.created, true).then_with(then),
        Sort::OldestFirst => crate::media::by_time(a.created, b.created, false).then_with(then),
        Sort::LastChangedFirst => {
            crate::media::by_time(a.modified, b.modified, true).then_with(then)
        }
        Sort::LongestUntouchedFirst => {
            crate::media::by_time(a.modified, b.modified, false).then_with(then)
        }
    }
}

/// What [`crate::media::Sort::Type`] groups a row by: the type the file would
/// be opened as, and nothing for a folder — which never reaches this, since
/// folders keep their own order in that listing.
fn kind_of_row(entry: &Entry) -> &str {
    entry.file().map(|file| file.mime).unwrap_or_default()
}

/// Whether a name answers to what was typed: the query anywhere in it, ignoring
/// case, which is the rule the shelves' own search uses.
fn matched(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

/// What the row above a listing says once it has been read.
///
/// The count, because a column the user is looking at the top of says nothing
/// about how far down it goes; and what was left out, when anything was, in the
/// same breath rather than in the log. A folder listed to [`MOST`] and quietly
/// cut off is a folder the shell is lying about.
fn note((folders, files): (usize, usize), left_out: usize) -> String {
    let plural = |count: usize, one: &str, many: &str| {
        if count == 1 {
            format!("{count} {one}")
        } else {
            format!("{count} {many}")
        }
    };
    let counted = match (folders, files) {
        (0, 0) => "Empty".to_string(),
        (0, files) => plural(files, "file", "files"),
        (folders, 0) => plural(folders, "folder", "folders"),
        (folders, files) => format!(
            "{}, {}",
            plural(folders, "folder", "folders"),
            plural(files, "file", "files")
        ),
    };
    match left_out {
        0 => counted,
        left_out => format!("{counted}, {left_out} more not shown"),
    }
}

/// How big a file is and when it was last written: `4.2 MiB · 12 March 2025`.
///
/// Both, because they are the two questions a list of files gets asked and
/// neither answers the other. A file the shell could not examine says nothing
/// about its date rather than claiming a wrong one — and nothing about its size
/// either, since the zero it came back with is the failure and not a fact.
fn facts_note(size: u64, modified: Option<SystemTime>) -> String {
    let size = crate::appinfo::human_size(size);
    match modified.and_then(date) {
        Some(when) => format!("{size} · {when}"),
        None => size,
    }
}

/// A time as this shell writes a date: `12 March 2025`.
///
/// Written out here rather than handed to `strftime`, for the reason the guide's
/// header is — the session's locale is not the language the rest of the shell is
/// in, and one Polish month in a column of English rows reads as a bug.
fn date(when: SystemTime) -> Option<String> {
    let secs = when.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let secs = secs.min(libc::time_t::MAX as u64) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r only reads `secs` and writes `tm`, both valid here.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    let month = crate::MONTHS.get(tm.tm_mon.clamp(0, 11) as usize)?;
    Some(format!("{} {month} {}", tm.tm_mday, tm.tm_year + 1900))
}

/// How much room is left on the filesystem `at` is on.
///
/// `None` where the question cannot be asked at all, which is what a path that
/// has gone away between being mounted and being looked at comes back as.
pub fn room(at: &Path) -> Option<String> {
    let path = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).ok()?;
    let mut facts: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string for the length of the
    // call, and `facts` is a live `statvfs` for the kernel to write into.
    if unsafe { libc::statvfs(path.as_ptr(), &mut facts) } != 0 {
        return None;
    }
    // `f_frsize` is the fragment size, which is what both counts are in.
    // `f_bsize` is the preferred size of an I/O block and is not the same
    // number on every filesystem there is.
    let unit = facts.f_frsize as u64;
    let whole = facts.f_blocks as u64 * unit;
    // What is left for this user, not what is left before the disk is full:
    // the difference is the reserve root keeps, and the person reading the row
    // is not root.
    let free = facts.f_bavail as u64 * unit;
    if whole == 0 {
        return None;
    }
    Some(format!(
        "{} free of {}",
        crate::appinfo::human_size(free),
        crate::appinfo::human_size(whole)
    ))
}

/// One mounted filesystem, as `/proc/mounts` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// What is mounted: a device node, or a network share.
    pub source: String,
    pub at: PathBuf,
    pub kind: String,
}

/// A drive the user can be offered, with the name to offer it under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    pub title: String,
    pub at: PathBuf,
}

/// The mount table, as the kernel keeps it.
fn read_mounts() -> Vec<Mount> {
    let Ok(raw) = std::fs::read_to_string("/proc/mounts") else {
        tracing::debug!("no mount table on this machine, so no drives on the bar");
        return Vec::new();
    };
    parse_mounts(&raw)
}

/// Split rather than parsed with anything: the format is five space-separated
/// fields and the only escaping in it is the octal one below.
fn parse_mounts(raw: &str) -> Vec<Mount> {
    raw.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let source = unescaped(fields.next()?);
            let at = unescaped(fields.next()?);
            let kind = fields.next()?.to_string();
            Some(Mount {
                source,
                at: PathBuf::from(at),
                kind,
            })
        })
        .collect()
}

/// `/proc/mounts` writes a space in a path as `\040`, and a backslash as
/// `\134`. A drive called `My Films` is mounted at a path with a space in it on
/// every machine that has one, so this is not an edge case, it is Tuesday.
fn unescaped(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut rest = field.chars();
    while let Some(c) = rest.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits: String = rest.clone().take(3).collect();
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) if digits.len() == 3 => {
                out.push(byte as char);
                rest.nth(2);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Filesystem types that are the kernel talking to itself rather than a disk
/// with somebody's files on it.
///
/// A deny list rather than an allow list, because the allow list is "every
/// filesystem Linux has ever had a driver for" and the deny list is this. What
/// gets past it still has to be backed by something — see [`drives`].
const PSEUDO: &[&str] = &[
    "autofs",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fuse.gvfsd-fuse",
    "fuse.portal",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "proc",
    "pstore",
    "ramfs",
    "securityfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// Filesystems that are somebody else's disk, reached over a network. Mounted
/// from a name rather than from a device node, which is why they need saying:
/// nothing else about them looks like a drive.
const NETWORK: &[&str] = &[
    "9p",
    "afs",
    "ceph",
    "cifs",
    "fuse.sshfs",
    "davfs",
    "fuse.davfs",
    "nfs",
    "nfs4",
    "smb3",
    "smbfs",
    "ssfs",
];

/// Where a drive that somebody plugged in gets mounted, on the desktops that
/// mount them. `udisks` uses the first, and the other two are where a person
/// mounts something by hand.
const REMOVABLE: &[&str] = &["/run/media", "/media", "/mnt"];

/// Mount points that are the machine's arrangement of itself rather than
/// anywhere somebody keeps things.
///
/// Every one of these is a real filesystem on a real disk and none of them is a
/// drive. On a machine whose root is btrfs they are usually subvolumes of the
/// disk that is already the Root row, so listing them would offer the same
/// drive five times under the names of its own directories; and `/home` is
/// worse than redundant, because the row above it is the user's own folder.
///
/// Matched as prefixes, so `/var/lib/whatever` goes with `/var`.
const SYSTEM: &[&str] = &[
    "/.snapshots",
    "/boot",
    "/efi",
    "/etc",
    "/home",
    "/nix",
    "/opt",
    "/root",
    "/run",
    "/snap",
    "/srv",
    "/swap",
    "/tmp",
    "/usr",
    "/var",
];

/// Which of the mounted filesystems are drives worth a row.
///
/// Four rules, and each is there to keep a particular kind of noise off the bar
/// rather than out of principle:
///
/// * It has to be a real filesystem — not one of the [`PSEUDO`] ones — mounted
///   from a device node or from a [`NETWORK`] share. That is most of it: a
///   stock session has around thirty mounts and all but a handful are the
///   kernel's own.
/// * `/` is left out, because it is the Root row and a machine offering its own
///   disk twice under two names is a machine explaining its mount table.
/// * One of the [`SYSTEM`] mount points is left out for the same reason.
/// * A loop device only counts under one of the [`REMOVABLE`] roots. Those are
///   `.iso` and `.img` files somebody attached, which is a drive; everywhere
///   else they are packaged applications mounted by the system, and a row per
///   installed snap is exactly the noise this is guarding against.
///
/// What is left is what somebody plugged in or mounted on purpose: the
/// removable roots, a network share, and a disk they gave a place of its own —
/// `/data`, `/games`. That last is why this is a list of what a machine mounts
/// for itself rather than a list of the three folders drives appear in: a
/// second disk somebody mounted at a name they chose is exactly the thing they
/// would go looking for here.
///
/// The name is the last part of where it is mounted, which on any desktop that
/// mounts drives is the volume's own label: `/run/media/kate/Photographs` is
/// "Photographs". A mount point with nothing to take a name from keeps its
/// whole path, which is at least true.
/// Where every real filesystem on this machine is mounted, `/` included.
///
/// A wider net than [`drives`] casts on purpose, because it is answering a
/// different question. That one asks "what would somebody call a disk", and
/// leaves out the machine's own arrangement of itself — `/usr`, `/var`, the
/// subvolumes of the root disk — because a bar offering the same drive five
/// times under the names of its own directories is a bar explaining its mount
/// table. This asks "where could a trash directory be", and a trash at the top
/// of a volume is at the top of the volume whatever the user would call it.
///
/// The pseudo filesystems are still left out, and so is anything not backed by
/// a device node or a network share: there is no trash on `/proc`, and walking
/// thirty of the kernel's own mounts looking for one is thirty `stat` calls
/// spent on a question with a known answer. See [`crate::trash::bins`], which
/// is the one caller.
pub fn mounted_volumes() -> Vec<PathBuf> {
    let mut tops: Vec<PathBuf> = Vec::new();
    for mount in read_mounts() {
        if PSEUDO.contains(&mount.kind.as_str()) {
            continue;
        }
        if !mount.source.starts_with("/dev/") && !NETWORK.contains(&mount.kind.as_str()) {
            continue;
        }
        // The same volume mounted twice is one volume — see [`drives`], which
        // guards the same thing for the same reason.
        if tops.contains(&mount.at) {
            continue;
        }
        tops.push(mount.at);
    }
    tops
}

pub fn drives(mounts: &[Mount]) -> Vec<Drive> {
    let mut drives: Vec<Drive> = Vec::new();
    for mount in mounts {
        let at = mount.at.as_path();
        if at == Path::new("/") || PSEUDO.contains(&mount.kind.as_str()) {
            continue;
        }
        // Before the system list rather than after it, because the one place a
        // desktop mounts a stick is inside one of the places a machine mounts
        // itself: `/run/media` is under `/run`.
        let removable = REMOVABLE.iter().any(|root| at.starts_with(root));
        if !removable && SYSTEM.iter().any(|own| at.starts_with(own)) {
            continue;
        }
        let block = mount.source.starts_with("/dev/");
        if mount.source.starts_with("/dev/loop") && !removable {
            continue;
        }
        if !block && !NETWORK.contains(&mount.kind.as_str()) {
            continue;
        }
        let title = at
            .file_name()
            .and_then(OsStr::to_str)
            .map(str::to_string)
            .unwrap_or_else(|| at.display().to_string());
        // The same drive mounted twice is one drive. It happens on any
        // filesystem with subvolumes, and on every machine where something has
        // been bind-mounted into place.
        if drives.iter().any(|drive| drive.at == mount.at) {
            continue;
        }
        drives.push(Drive {
            title,
            at: mount.at.clone(),
        });
    }
    drives.sort_by_key(|drive| drive.title.to_lowercase());
    drives
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

/// What the shell will say a file is, by its extension: the type it would be
/// handed to an application as, and the mark it is drawn with.
///
/// The extension and nothing else, for the reason the shelves give: deciding by
/// content means opening every file in the folder to read four bytes out of it,
/// and a listing is not somewhere to spend a thousand `open` calls.
///
/// The three kinds the shelves already know are asked for first, so a song is
/// drawn as a song here as well — one file, one mark, whichever column it is
/// being looked at from. What is left is this table: the types somebody
/// actually has in a folder, with the marks kept deliberately few. A page is
/// the honest drawing for a file whose only distinguishing feature is its name.
///
/// An archive is the one thing in that remainder with a mark of its own, and it
/// is here on the same rule the three shelved kinds are: the shell *knows* what
/// one is, because it opens one itself. Which types those are is
/// [`crate::archive::opens`]'s list and not a second copy of it kept here — a
/// file drawn as a box that then turned out to have nothing that could open it
/// would be a row promising something the press cannot deliver.
pub fn described(path: &Path) -> (&'static str, &'static str) {
    if let Some(kind) = crate::media::kind_of(path) {
        return (crate::media::mime_of(path).unwrap_or(OCTETS), kind.glyph());
    }
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = TYPES
        .iter()
        .find(|(name, _)| *name == extension)
        .map(|(_, mime)| *mime)
        .unwrap_or(OCTETS);
    let glyph = if crate::archive::opens(mime) {
        crate::icons::FILE_ARCHIVE
    } else {
        crate::icons::FILE_PAGE
    };
    (mime, glyph)
}

/// What a file nothing recognises is: some bytes. Named rather than written out
/// at each of its uses, because it is an answer and not a fallback — the type
/// is genuinely unknown, and saying so is what lets `xdg-open` be asked.
const OCTETS: &str = "application/octet-stream";

/// Everything that is not a song, a film or a photograph, and is still worth
/// naming.
///
/// One line per type the user is likely to press. It is not a complete mime
/// database and it is not trying to be one — `/usr/share/mime` is that, it is
/// four megabytes of XML, and what this table is for is telling the desktop
/// which application to open a document with.
const TYPES: &[(&str, &str)] = &[
    // Text, and the things that are text underneath.
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("log", "text/plain"),
    ("conf", "text/plain"),
    ("cfg", "text/plain"),
    ("ini", "text/plain"),
    ("toml", "text/plain"),
    ("yaml", "text/plain"),
    ("yml", "text/plain"),
    ("json", "application/json"),
    ("xml", "application/xml"),
    ("csv", "text/csv"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("css", "text/css"),
    ("desktop", "application/x-desktop"),
    // Documents.
    ("pdf", "application/pdf"),
    ("epub", "application/epub+zip"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("doc", "application/msword"),
    ("rtf", "application/rtf"),
    // Archives and images of disks.
    //
    // A tarball whose compression is folded into one extension is named as the
    // compression it wears, which is what `foo.tar.gz` already comes out as:
    // `Path::extension` gives `gz` and nothing here sees the `.tar` in the
    // middle. So `foo.tgz` and `foo.tar.gz` are one type, as they should be —
    // and what tells the two shapes apart when it matters is the whole name,
    // which is [`crate::archive::unpack`]'s question rather than this one's.
    ("zip", "application/zip"),
    ("tar", "application/x-tar"),
    ("gz", "application/gzip"),
    ("tgz", "application/gzip"),
    ("bz2", "application/x-bzip2"),
    ("tbz", "application/x-bzip2"),
    ("tbz2", "application/x-bzip2"),
    ("xz", "application/x-xz"),
    ("txz", "application/x-xz"),
    ("lzma", "application/x-lzma"),
    ("zst", "application/zstd"),
    ("tzst", "application/zstd"),
    ("lz4", "application/x-lz4"),
    ("lz", "application/x-lzip"),
    ("tlz", "application/x-lzip"),
    ("7z", "application/x-7z-compressed"),
    ("rar", "application/vnd.rar"),
    ("cab", "application/vnd.ms-cab-compressed"),
    ("iso", "application/x-cd-image"),
    ("img", "application/x-raw-disk-image"),
    // What a distribution is made of.
    ("deb", "application/vnd.debian.binary-package"),
    ("rpm", "application/x-rpm"),
    ("pkg.tar.zst", "application/zstd"),
    ("appimage", "application/x-iso9660-appimage"),
    ("flatpakref", "application/vnd.flatpak.ref"),
    // Programs the user wrote.
    ("sh", "application/x-shellscript"),
    ("py", "text/x-python"),
    ("rs", "text/rust"),
    ("c", "text/x-csrc"),
    ("h", "text/x-chdr"),
    ("cpp", "text/x-c++src"),
    ("js", "text/javascript"),
    ("ts", "text/x-typescript"),
    ("patch", "text/x-patch"),
    ("diff", "text/x-patch"),
    // Fonts, which are worth a row because a folder of them is a thing people
    // have and every desktop has something that previews one.
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
];

/// Whether any file this shell lists can be described as being of this type.
///
/// Asked by [`crate::archive`]'s tests, which claim a list of types and must
/// not claim one the table above never produces: a handler offered for a type
/// no row can carry is an offer nothing could ever test. Here rather than
/// there because the table is here, and because a second copy of it kept
/// somewhere else is a second copy to keep in step.
///
/// Only under test. Nothing the shell does while it is running needs to ask
/// this — the type of a file is [`described`]'s answer, and the question here
/// is about the table itself.
#[cfg(test)]
pub fn names_a_type(mime: &str) -> bool {
    TYPES.iter().any(|(_, named)| *named == mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own under the system's temporary folder, or
    /// `None` where there is nowhere to write — in which case the test that
    /// wanted it says nothing rather than failing.
    fn scratch(name: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lxb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// What the column holds under the rows that stand over it. The field at
    /// the head is not part of the listing, and every one of these tests is
    /// about the listing.
    fn titles(rows: &[Entry]) -> Vec<&str> {
        rows[crate::apps::head_rows(rows)..]
            .iter()
            .map(Entry::title)
            .collect()
    }

    /// The rows a folder comes back with, unsearched and in the order every
    /// column opens in.
    fn shown(at: &Path) -> Shown {
        listing(at, "", How::plain(), Shows::Everything)
    }

    /// An archive is drawn as the box it is, and everything the shell has no
    /// opinion about is still a page.
    ///
    /// The two halves of one rule: a mark is for a kind the shell can actually
    /// do something with. A song keeps its shelf's mark because there is a
    /// shelf; an archive gets the carton because there is an Extract; a `.pdf`
    /// gets the page, because what it is, is written on the row in words.
    #[test]
    fn an_archive_wears_the_box_and_a_document_wears_the_page() {
        let mark = |name: &str| described(Path::new(name)).1;
        assert_eq!(mark("/x/holiday.tar.gz"), crate::icons::FILE_ARCHIVE);
        assert_eq!(mark("/x/photos.ZIP"), crate::icons::FILE_ARCHIVE);
        assert_eq!(mark("/x/linux.pkg.tar.zst"), crate::icons::FILE_ARCHIVE);
        assert_eq!(mark("/x/disc.iso"), crate::icons::FILE_ARCHIVE);

        assert_eq!(mark("/x/notes.pdf"), crate::icons::FILE_PAGE);
        assert_eq!(mark("/x/thing"), crate::icons::FILE_PAGE);
        // A raw disk image is not a box of files and nothing here unpacks one.
        assert_eq!(mark("/x/card.img"), crate::icons::FILE_PAGE);
        // And the three the shelves already know keep their own marks.
        assert_eq!(mark("/x/song.flac"), crate::media::Kind::Audio.glyph());
    }

    /// Every type drawn as a box has something behind the press, and every type
    /// Extract opens is drawn as one. The row and the press must not be able to
    /// disagree about what a file is.
    #[test]
    fn the_box_and_the_press_are_the_same_list() {
        for (extension, mime) in TYPES {
            let drawn = described(Path::new(&format!("/x/thing.{extension}"))).1;
            assert_eq!(
                drawn == crate::icons::FILE_ARCHIVE,
                crate::archive::opens(mime),
                "{extension} is drawn {drawn} and Extract {} open it",
                if crate::archive::opens(mime) {
                    "does"
                } else {
                    "does not"
                }
            );
        }
    }

    /// A column opened to choose a wallpaper lists the folders to keep walking
    /// through and the two kinds of file that can be drawn, and nothing else.
    ///
    /// A walk that is choosing one of a game's pictures leaves the films out.
    ///
    /// The one difference from the wallpaper's own walk, and it is a fact about
    /// what the answer is *for*: a wallpaper can be a film, and neither of a
    /// game's two pictures can — what stands behind a game is a still, and a
    /// cover is a card an inch high. A row offering a film would be an answer
    /// this shell cannot use.
    ///
    /// Everything else about it is the wallpaper's walk: the folders are there
    /// to keep walking through, and stepping into one goes on choosing rather
    /// than browsing.
    #[test]
    fn a_walk_for_a_games_picture_leaves_the_films_out() {
        let Some(dir) = scratch("game-picture") else {
            return;
        };
        std::fs::create_dir(dir.join("Albums")).unwrap();
        for name in ["cover.png", "holiday.mp4", "notes.txt", "song.flac"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let shows = Shows::Picture(crate::retroarch::Piece::Cover);
        let picking = listing(&dir, "", How::plain(), shows);
        assert_eq!(
            titles(&picking.rows),
            ["Albums", "cover.png"],
            "a folder to walk into, and the one picture"
        );

        let Some(Entry::Folder(folder)) = picking
            .rows
            .iter()
            .find(|row| row.title() == "Albums")
            .cloned()
        else {
            panic!("the folder is a way further in");
        };
        assert_eq!(folder.place.as_ref().map(Place::shows), Some(shows));
        assert_eq!(
            shows.piece(),
            Some(crate::retroarch::Piece::Cover),
            "and every step of it still knows which picture it is choosing"
        );
        assert_eq!(shows.picking(), None, "it is not a folder being chosen");
    }

    /// The row above it counts what it is showing rather than what is in the
    /// folder, because that row is a description of the column somebody is
    /// looking at.
    #[test]
    fn a_wallpaper_column_lists_only_what_could_stand_behind_a_screen() {
        let Some(dir) = scratch("scenery") else {
            return;
        };
        std::fs::create_dir(dir.join("Albums")).unwrap();
        for name in [
            "sunset.jpg",
            "drawing.svg",
            "holiday.mp4",
            "notes.txt",
            "song.flac",
            "archive.tar.zst",
            "unknowable",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let picking = listing(&dir, "", How::plain(), Shows::Scenery);
        assert_eq!(
            titles(&picking.rows),
            ["Albums", "drawing.svg", "holiday.mp4", "sunset.jpg"],
            "a folder to walk into, a film, and two pictures"
        );
        assert_eq!(
            picking.note, "1 folder, 3 files",
            "and the count is of the column, not of the directory"
        );

        // The same folder under Files is the folder as it really is: this is a
        // property of the walk, not a filter somebody turned on everywhere.
        assert_eq!(titles(&shown(&dir).rows).len(), 8);

        // And stepping further in keeps choosing rather than browsing — the
        // whole walk is one question. See `Shows`.
        let Some(Entry::Folder(folder)) = picking
            .rows
            .iter()
            .find(|row| row.title() == "Albums")
            .cloned()
        else {
            panic!("the folder is a way further in");
        };
        assert_eq!(
            folder.place.as_ref().map(Place::shows),
            Some(Shows::Scenery)
        );

        // As do the three disks the picker opens on, which are the same three
        // Files opens on.
        for row in volumes("", Shows::Scenery).rows {
            let Entry::Folder(place) = row else { continue };
            assert_eq!(place.place.as_ref().map(Place::shows), Some(Shows::Scenery));
        }
    }

    #[test]
    fn folders_come_before_files_and_both_are_alphabetical() {
        let Some(dir) = scratch("listing") else {
            return;
        };
        for folder in ["zebra", "Ada"] {
            std::fs::create_dir(dir.join(folder)).unwrap();
        }
        for file in ["b.txt", "A.txt"] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        let found = shown(&dir);
        assert_eq!(titles(&found.rows), ["Ada", "zebra", "A.txt", "b.txt"]);
        assert_eq!(found.note, "2 folders, 2 files");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder is a way further in and carries the path to walk; a file is a
    /// leaf and carries none.
    #[test]
    fn only_the_folders_lead_anywhere() {
        let Some(dir) = scratch("leads") else {
            return;
        };
        std::fs::create_dir(dir.join("inside")).unwrap();
        std::fs::write(dir.join("note.txt"), b"x").unwrap();
        let rows = shown(&dir).rows;
        let rows = &rows[crate::apps::head_rows(&rows)..];
        match &rows[0] {
            Entry::Folder(folder) => assert_eq!(
                folder.place,
                Some(Place::Directory(dir.join("inside"), Shows::Everything)),
                "a folder is stepped into"
            ),
            other => panic!("expected a folder, got {other:?}"),
        }
        assert!(matches!(&rows[1], Entry::File(_)), "a file is a leaf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dotfiles_are_left_out() {
        let Some(dir) = scratch("dotfiles") else {
            return;
        };
        std::fs::create_dir(dir.join(".config")).unwrap();
        std::fs::write(dir.join(".bashrc"), b"x").unwrap();
        std::fs::write(dir.join("seen.txt"), b"x").unwrap();
        let found = shown(&dir);
        assert_eq!(titles(&found.rows), ["seen.txt"]);
        assert_eq!(found.note, "1 file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And they are there for the asking, with the folders still over the files
    /// and the count above saying so — the switch changes which rows a folder
    /// has, not how a folder is read otherwise.
    #[test]
    fn the_hidden_names_are_listed_when_the_switch_is_on() {
        let Some(dir) = scratch("hidden-shown") else {
            return;
        };
        std::fs::create_dir(dir.join(".config")).unwrap();
        std::fs::write(dir.join(".bashrc"), b"x").unwrap();
        std::fs::write(dir.join("seen.txt"), b"x").unwrap();
        let how = How {
            hidden: true,
            ..How::plain()
        };
        let found = listing(&dir, "", how, Shows::Everything);
        assert_eq!(
            titles(&found.rows),
            [".config", ".bashrc", "seen.txt"],
            "the folder first, whatever its name begins with"
        );
        assert_eq!(found.note, "1 folder, 2 files");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The field narrows what the switch has let through, and nothing else: a
    /// search made with the hidden names off cannot find one.
    #[test]
    fn a_search_reaches_a_hidden_name_only_where_they_are_listed() {
        let Some(dir) = scratch("hidden-search") else {
            return;
        };
        std::fs::write(dir.join(".bashrc"), b"x").unwrap();
        std::fs::write(dir.join("bash-notes.txt"), b"x").unwrap();
        let hidden = How {
            hidden: true,
            ..How::plain()
        };
        assert_eq!(
            titles(&listing(&dir, "bash", How::plain(), Shows::Everything).rows),
            ["bash-notes.txt"],
        );
        assert_eq!(
            titles(&listing(&dir, "bash", hidden, Shows::Everything).rows),
            [".bashrc", "bash-notes.txt"],
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The trash, as a column: the row that empties it over everything in it,
    /// newest first.
    #[test]
    fn the_trash_carries_the_row_that_empties_it_over_what_is_in_it() {
        let listing = vec![
            trashed("holiday.mp4", "2026-08-20T10:00:00", false),
            trashed("Live at Leeds", "2026-08-24T18:30:00", true),
            trashed("notes.txt", "2026-08-22T09:15:00", false),
        ];
        let shown = trash_of(listing.clone(), crate::media::Sort::NewestFirst);

        let rows: Vec<&str> = shown.rows.iter().map(Entry::title).collect();
        assert_eq!(
            rows,
            ["Empty trash", "Live at Leeds", "notes.txt", "holiday.mp4"],
            "newest first, and folders are not lifted above files: in the trash \
             nothing leads anywhere, and what somebody is looking for is what \
             they deleted by mistake"
        );
        assert!(shown.rows[0].over_the_list(), "it stands over the list");
        assert_eq!(shown.rows[0].comment(), Some("3 items"));
        assert_eq!(shown.note, "1 folder, 2 files");

        // Every other order is the shell's own, on what the trash knows.
        let by_name = trash_of(listing.clone(), crate::media::Sort::NameAscending);
        let named: Vec<&str> = by_name.rows[1..].iter().map(Entry::title).collect();
        assert_eq!(named, ["holiday.mp4", "Live at Leeds", "notes.txt"]);

        // An empty trash gets no row at all, so the column cannot be stepped
        // into — the row that opened it has already said "Empty", and a
        // control with nothing to act on is worse than no control.
        let nothing = trash_of(Vec::new(), crate::media::Sort::NewestFirst);
        assert!(nothing.rows.is_empty());
        assert_eq!(nothing.note, "Empty");
    }

    /// A row in the trash is named for what it was called before it was
    /// deleted, not for what the trash filed it as.
    #[test]
    fn a_trashed_row_wears_the_name_the_user_gave_it() {
        let shown = trash_of(
            vec![trashed("Don't Stop.mp3", "2026-08-24T18:30:00", false)],
            crate::media::Sort::NewestFirst,
        );
        assert_eq!(shown.rows[1].title(), "Don't Stop.mp3");
        assert_eq!(
            shown.rows[1].comment(),
            Some("From /home/x/Music · deleted 24 August 2026")
        );
        assert_eq!(
            shown.rows[1].icon(),
            Some(crate::media::Kind::Audio.glyph())
        );
    }

    /// One thing in the trash, as `crate::trash` would have read it.
    fn trashed(name: &str, when: &str, folder: bool) -> crate::trash::Trashed {
        let from = PathBuf::from(format!("/home/x/Music/{name}"));
        let mut item = crate::trash::Trashed {
            name: name.to_string(),
            at: PathBuf::from(format!("/home/x/.local/share/Trash/files/{name}")),
            ticket: PathBuf::from(format!("/home/x/.local/share/Trash/info/{name}.trashinfo")),
            from,
            note: String::new(),
            deleted_at: when.to_string(),
            folder,
            size: 0,
            modified: None,
        };
        item.note = item.describe();
        item
    }

    /// A directory the user may not read is not a directory with nothing in it,
    /// and the row says which.
    #[test]
    fn an_unreadable_folder_says_so() {
        let found = shown(Path::new("/proc/1/fdinfo/nothing-here"));
        assert!(found.rows.is_empty());
        assert_eq!(found.note, "This cannot be opened");
    }

    /// An empty folder carries exactly one row: the offer to make something in
    /// it.
    ///
    /// No field — there is nothing there to search — and that used to be the
    /// whole of the column, which made an empty directory a dead end: a column
    /// with nothing in it cannot be stepped into, so the first thing in one had
    /// to be made from a terminal. The head row is what opens it.
    #[test]
    fn an_empty_folder_can_still_be_stepped_into_to_make_something_in_it() {
        let Some(dir) = scratch("empty") else {
            return;
        };
        let found = shown(&dir);
        let rows: Vec<&str> = found.rows.iter().map(Entry::title).collect();
        assert_eq!(rows, ["New folder"]);
        assert!(found.rows[0].over_the_list(), "it stands over the list");
        assert!(titles(&found.rows).is_empty(), "and nothing under it");
        assert_eq!(found.note, "Empty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And a folder this user cannot write to carries no such row: `/usr/lib`
    /// is four presses from Root, and an offer nobody on the machine can take
    /// standing over six thousand rows is furniture rather than a command.
    #[test]
    fn a_folder_that_cannot_be_written_to_is_offered_no_way_to_make_one() {
        let found = shown(Path::new("/usr/lib"));
        assert!(
            !found.rows.iter().any(|row| row.make().is_some()),
            "{:?}",
            titles(&found.rows).first()
        );
    }

    /// A link to a directory is a way further in, not a file. `/lib` is one on
    /// every merged-usr system, so a Root column that got this wrong would have
    /// three unopenable rows at the top of it.
    #[test]
    fn a_link_to_a_folder_is_a_folder() {
        let Some(dir) = scratch("links") else {
            return;
        };
        std::fs::create_dir(dir.join("real")).unwrap();
        if std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).is_err() {
            return;
        }
        let rows = shown(&dir).rows;
        assert_eq!(titles(&rows), ["link", "real"]);
        assert!(rows[crate::apps::head_rows(&rows)..]
            .iter()
            .all(|row| row.entries().is_some()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file keeps its extension, which is half of what the user came to the
    /// folder to see. The shelves drop it, deliberately, and this is the other
    /// half of that decision.
    #[test]
    fn a_file_keeps_its_whole_name() {
        let Some(dir) = scratch("names") else {
            return;
        };
        std::fs::write(dir.join("Yesterday.flac"), b"x").unwrap();
        assert_eq!(titles(&shown(&dir).rows), ["Yesterday.flac"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The field narrows the column, and the row above goes on counting the
    /// whole folder: one says what is being shown, the other what is there.
    #[test]
    fn a_search_keeps_what_answers_to_it() {
        let Some(dir) = scratch("search") else {
            return;
        };
        std::fs::create_dir(dir.join("Reports")).unwrap();
        for file in ["report.pdf", "notes.txt", "REPORT-2.pdf"] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        let found = listing(&dir, "report", How::plain(), Shows::Everything);
        assert_eq!(
            titles(&found.rows),
            ["Reports", "REPORT-2.pdf", "report.pdf"],
            "anywhere in the name, ignoring case"
        );
        assert_eq!(
            found.note, "1 folder, 3 files",
            "the row above says what is in the folder, not what is being shown"
        );

        let head = &found.rows[..crate::apps::head_rows(&found.rows)];
        let notes: Vec<Option<&str>> = head.iter().map(Entry::comment).collect();
        assert_eq!(
            notes,
            [
                Some("Make a folder in this one"),
                Some("3 of 4 items match"),
                Some("Show all 4 items")
            ],
            "the field says what it kept, and under it the way back — with the \
             row that makes a folder standing over both, where the picker's own \
             answer row stands"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder narrowed to nothing still carries the two rows that say why.
    /// A user left looking at an empty column would have no way back to their
    /// own files but the one they could not see.
    #[test]
    fn a_search_that_finds_nothing_keeps_its_field() {
        let Some(dir) = scratch("nothing") else {
            return;
        };
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let found = listing(&dir, "zzz", How::plain(), Shows::Everything);
        assert!(titles(&found.rows).is_empty());
        // New folder, the field, and the row that empties it.
        assert_eq!(crate::apps::head_rows(&found.rows), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The nine orders, on a directory. Folders stay above files in all of
    /// them, and in the two the directory cannot answer for — size, type — they
    /// keep the order somebody can find them in.
    #[test]
    fn a_folder_is_listed_in_the_order_that_was_asked_for() {
        use crate::media::Sort;
        let Some(dir) = scratch("orders") else {
            return;
        };
        for folder in ["Ada", "zebra"] {
            std::fs::create_dir(dir.join(folder)).unwrap();
        }
        std::fs::write(dir.join("big.bin"), vec![0u8; 4096]).unwrap();
        std::fs::write(dir.join("small.txt"), b"x").unwrap();

        let order = |sort| {
            let how = How {
                sort,
                ..How::plain()
            };
            titles(&listing(&dir, "", how, Shows::Everything).rows).join(" ")
        };
        assert_eq!(order(Sort::NameAscending), "Ada zebra big.bin small.txt");
        assert_eq!(order(Sort::NameDescending), "zebra Ada small.txt big.bin");
        assert_eq!(
            order(Sort::LargestFirst),
            "Ada zebra big.bin small.txt",
            "the folders keep their own order in an order they have no answer for"
        );
        assert_eq!(order(Sort::SmallestFirst), "Ada zebra small.txt big.bin");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a listing can be put in order by is a question about the
    /// filesystem it came off, and only the read that produced it can answer.
    #[test]
    fn a_listing_says_what_it_could_be_ordered_by() {
        let Some(dir) = scratch("orders-known") else {
            return;
        };
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let found = listing(&dir, "", How::plain(), Shows::Everything);
        assert!(
            found.orders.modified,
            "every Unix filesystem keeps a modification time"
        );

        let empty = listing(&dir, "zzz", How::plain(), Shows::Everything);
        assert_eq!(
            empty.orders,
            crate::media::Orders::default(),
            "nothing was read, so nothing is known"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_song_in_a_folder_keeps_the_shelf_mark() {
        let (mime, glyph) = described(Path::new("/music/a.flac"));
        assert_eq!(mime, "audio/flac");
        assert_eq!(glyph, crate::icons::CATEGORY_MUSIC);
    }

    #[test]
    fn a_document_is_named_and_a_stranger_is_not() {
        assert_eq!(described(Path::new("/a/b.pdf")).0, "application/pdf");
        assert_eq!(described(Path::new("/a/b.PDF")).0, "application/pdf");
        assert_eq!(described(Path::new("/a/b.wibble")).0, OCTETS);
        assert_eq!(described(Path::new("/a/README")).0, OCTETS);
        assert_eq!(described(Path::new("/a/b.pdf")).1, crate::icons::FILE_PAGE);
    }

    #[test]
    fn the_mount_table_is_read_as_the_kernel_writes_it() {
        let raw = "\
proc /proc proc rw,nosuid 0 0
/dev/nvme0n1p2 / btrfs rw,relatime 0 0
/dev/sdb1 /run/media/kate/My\\040Films exfat rw,nosuid 0 0
";
        let mounts = parse_mounts(raw);
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[2].at, PathBuf::from("/run/media/kate/My Films"));
        assert_eq!(mounts[2].kind, "exfat");
    }

    /// The whole of the drive rule, on one machine's worth of mount table.
    #[test]
    fn only_the_drives_somebody_would_browse_get_a_row() {
        let raw = "\
proc /proc proc rw 0 0
sysfs /sys sysfs rw 0 0
tmpfs /run tmpfs rw 0 0
/dev/nvme0n1p2 / btrfs rw,subvol=/@ 0 0
/dev/nvme0n1p2 /home btrfs rw,subvol=/@home 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
/dev/loop0 /var/lib/snapd/snap/core/1 squashfs ro 0 0
/dev/loop3 /run/media/kate/Install exfat ro 0 0
/dev/sdb1 /run/media/kate/Photographs exfat rw 0 0
nas:/export/films /mnt/films nfs4 rw 0 0
/dev/sdc1 /games ext4 rw 0 0
";
        let drives = drives(&parse_mounts(raw));
        let names: Vec<&str> = drives.iter().map(|drive| drive.title.as_str()).collect();
        assert_eq!(names, ["films", "games", "Install", "Photographs"]);
    }

    #[test]
    fn a_drive_mounted_twice_is_one_row() {
        let raw = "\
/dev/sdb1 /run/media/kate/Stick exfat rw 0 0
/dev/sdb1 /run/media/kate/Stick exfat rw 0 0
";
        assert_eq!(drives(&parse_mounts(raw)).len(), 1);
    }

    /// Counting is the row above a listing, so the sentence has to be right for
    /// one of a thing as well as for none and for many.
    #[test]
    fn a_listing_counts_what_is_in_it() {
        assert_eq!(note((0, 0), 0), "Empty");
        assert_eq!(note((1, 0), 0), "1 folder");
        assert_eq!(note((0, 1), 0), "1 file");
        assert_eq!(note((2, 3), 0), "2 folders, 3 files");
        assert_eq!(note((0, 2), 5), "2 files, 5 more not shown");
    }

    #[test]
    fn a_date_is_written_out_in_english() {
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_741_000_000);
        let written = date(when).expect("the epoch converts");
        assert!(
            written.ends_with("March 2025"),
            "expected a March 2025 date, got {written}"
        );
    }
}
