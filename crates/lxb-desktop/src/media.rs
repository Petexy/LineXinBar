//! The user's own music, films and photographs, found under their home
//! directory.
//!
//! Multimedia's two rows used to be waiting for applications that could never
//! be filed into them — the menu spec cannot say which half of the column a
//! player belongs to, so nothing was ever put there. They hold something the
//! machine *can* answer for instead: the audio and video files the user
//! actually has. A row per file, alphabetical, gathered from everywhere under
//! `$HOME` rather than from the one folder that happens to be called Music.
//!
//! Graphics carries a third row on the same terms, for the same reason and out
//! of the same walk: a picture is a thing the user has rather than a thing
//! that is installed, and one pass over the home directory answers for all
//! three kinds at once.
//!
//! ## Why it is on a thread
//!
//! A home directory is not a small place. Walking one with a few hundred
//! thousand files in it takes seconds on a cold cache, and doing that when the
//! user steps into Music would mean the column opened onto nothing and then
//! filled in — the one thing a launcher must never do, because a list that
//! arrives late is a list the user has already given up on.
//!
//! So the walk starts when the shell does and runs on a worker for as long as
//! the session lasts. What it finds is handed over a channel and hung on the
//! bar by the frame that was going to be drawn anyway, exactly as the package
//! manager's answers and the volume worker's readings are. By the time anyone
//! reaches Multimedia the column is normally already full, and if it is not,
//! it fills in front of them rather than making them wait.
//!
//! The walk is repeated every [`AGAIN`] rather than watched with inotify. A
//! watch is one kernel object per directory and there is no bound on how many
//! directories a home holds; a re-walk costs one `readdir` per folder on a
//! cache that is by then warm, and a song copied in during a session shows up
//! within a few minutes. Files that have gone are taken back off the same way.
//!
//! ## What does not wait for the next pass
//!
//! Five minutes is the right interval for a file that appeared while nobody
//! was looking, and the wrong one for the two cases where somebody *is*:
//!
//! - **A file this session made.** The shell knows the exact path of a
//!   screenshot the moment the compositor answers for it, so that one file is
//!   handed straight to the worker — see [`Library::found`]. It costs one
//!   `stat` and it is the difference between a picture appearing in Images as
//!   it is taken and appearing some minutes later.
//! - **A shelf somebody has just stepped into.** Opening Music, Video or
//!   Images is a person asking what they have, so it brings the next pass
//!   forward — see [`Library::look_again`]. Not a scan of its own: it is the
//!   same walk, started early, so a session where somebody bounces in and out
//!   of a column cannot cost more than one pass per [`SOON`].

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::apps::{App, Category};

/// How deep under the home directory the walk goes.
///
/// Deep enough for `Music/artist/album/disc 2`, and finite so a directory
/// tree that loops back on itself through a bind mount cannot walk forever.
/// Symbolic links are not followed at all, which is the other half of that.
const DEPTH: usize = 12;

/// How long after finishing a pass before the next one starts.
const AGAIN: Duration = Duration::from_secs(300);

/// The soonest after a pass that stepping into a shelf may start another.
///
/// The floor under [`Library::look_again`], and the whole of what stops it
/// being expensive: without one, walking in and out of Images ten times would
/// be ten walks of the home directory. With it, the tenth costs nothing —
/// they are all answered by the one pass that was already going to happen.
///
/// Twenty seconds because that is about how long it takes to do the thing the
/// user stepped out to do — take a screenshot, save a recording — and come
/// back. Anything they made faster than that is already on the shelf: the
/// shell hands over its own files as it makes them rather than waiting for a
/// walk to find them.
const SOON: Duration = Duration::from_secs(20);

/// How often what has been found so far is hung on the bar during a pass.
///
/// Everything that costs anything in proportion to the size of the collection
/// happens here and nowhere else: what the walk reports is held aside until
/// this comes round, and then merged, ordered and hung on the bar in one go.
/// So a frame costs the same whether the user has a hundred songs or a hundred
/// thousand, and the merging costs four passes a second while the walk runs
/// rather than one per frame. Four times a second is also far below what
/// anyone can see arriving.
const PUBLISH: Duration = Duration::from_millis(250);

/// Which row of which column a file belongs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Audio,
    Video,
    Image,
}

impl Kind {
    /// The glyph a file of this kind is drawn with.
    ///
    /// The same drawing as the row it hangs under, which is deliberate: the
    /// note *is* the mark for a piece of music, and a second, subtly different
    /// note for the tracks inside the note-headed row would be a distinction
    /// with nothing behind it. It is also what the console did — a track with
    /// no cover art is drawn as the mark of the column it is in.
    pub fn glyph(self) -> &'static str {
        match self {
            Kind::Audio => crate::icons::CATEGORY_MUSIC,
            Kind::Video => crate::icons::CATEGORY_VIDEO,
            Kind::Image => crate::icons::CATEGORY_IMAGES,
        }
    }

    /// What a list of these is called, in a sentence.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Audio => "audio files",
            Kind::Video => "video files",
            Kind::Image => "images",
        }
    }

    fn singular(self) -> &'static str {
        match self {
            Kind::Audio => "audio file",
            Kind::Video => "video file",
            Kind::Image => "image",
        }
    }

    /// Whether a file of this kind has a picture in it worth showing in place
    /// of the glyph — see [`crate::thumbs`].
    ///
    /// Music does not. There is cover art inside a great many audio files, but
    /// getting at it means parsing a tag format per container, and a note is
    /// not a bad answer for a track: what a listener picks a song by is its
    /// name. A film and a photograph are the opposite — the name is the weak
    /// half and the picture is the strong one.
    pub fn has_picture(self) -> bool {
        matches!(self, Kind::Video | Kind::Image)
    }
}

/// Every extension the shell will put on the bar, and what it is.
///
/// An extension rather than a sniffed header: the walk touches directory
/// entries only, and opening a quarter of a million files to read four bytes
/// out of each is not a background task, it is the disk for the next hour.
///
/// The mime type is here because the file has to be *opened* as well as
/// listed, and what opens it is whatever the user has already chosen for that
/// type — see [`opening`]. Where a container carries either kind, the entry
/// says which of the two it is normally used for: `.mka` is Matroska with only
/// audio in it, `.ogg` is Vorbis far more often than Theora.
///
/// `.ts` and `.mts` are deliberately absent. Both name MPEG transport streams
/// and both name TypeScript sources, and a developer's home directory would
/// otherwise put several thousand of the second kind under Video. `.m2ts` is
/// unambiguous and is here.
///
/// `.ico`, `.xpm`, `.pbm` and the rest of the icon and dump formats are absent
/// on a different ground: they are unambiguous, and they are not pictures
/// anybody keeps. A shelf of photographs with a window-manager icon set filed
/// into it is a worse answer than one that leaves them out. `.svg` is the hard
/// case and it is *in*: it is as much an icon format as a drawing format, but
/// a drawing the user made is a picture they own, and leaving it out to keep
/// somebody's icon theme off the shelf would be deciding what their own files
/// are for.
const TYPES: &[(&str, Kind, &str)] = &[
    ("mp3", Kind::Audio, "audio/mpeg"),
    ("flac", Kind::Audio, "audio/flac"),
    ("ogg", Kind::Audio, "audio/ogg"),
    ("oga", Kind::Audio, "audio/ogg"),
    ("opus", Kind::Audio, "audio/opus"),
    ("m4a", Kind::Audio, "audio/mp4"),
    ("m4b", Kind::Audio, "audio/x-m4b"),
    ("aac", Kind::Audio, "audio/aac"),
    ("wav", Kind::Audio, "audio/x-wav"),
    ("wma", Kind::Audio, "audio/x-ms-wma"),
    ("aiff", Kind::Audio, "audio/x-aiff"),
    ("aif", Kind::Audio, "audio/x-aiff"),
    ("ape", Kind::Audio, "audio/x-ape"),
    ("wv", Kind::Audio, "audio/x-wavpack"),
    ("mpc", Kind::Audio, "audio/x-musepack"),
    ("mka", Kind::Audio, "audio/x-matroska"),
    ("mp4", Kind::Video, "video/mp4"),
    ("m4v", Kind::Video, "video/x-m4v"),
    ("mkv", Kind::Video, "video/x-matroska"),
    ("webm", Kind::Video, "video/webm"),
    ("avi", Kind::Video, "video/x-msvideo"),
    ("divx", Kind::Video, "video/x-msvideo"),
    ("mov", Kind::Video, "video/quicktime"),
    ("wmv", Kind::Video, "video/x-ms-wmv"),
    ("flv", Kind::Video, "video/x-flv"),
    ("mpg", Kind::Video, "video/mpeg"),
    ("mpeg", Kind::Video, "video/mpeg"),
    ("vob", Kind::Video, "video/mpeg"),
    ("ogv", Kind::Video, "video/ogg"),
    ("3gp", Kind::Video, "video/3gpp"),
    ("m2ts", Kind::Video, "video/mp2t"),
    ("rmvb", Kind::Video, "application/vnd.rn-realmedia-vbr"),
    ("jpg", Kind::Image, "image/jpeg"),
    ("jpeg", Kind::Image, "image/jpeg"),
    ("png", Kind::Image, "image/png"),
    ("gif", Kind::Image, "image/gif"),
    ("webp", Kind::Image, "image/webp"),
    ("avif", Kind::Image, "image/avif"),
    ("jxl", Kind::Image, "image/jxl"),
    ("heic", Kind::Image, "image/heif"),
    ("heif", Kind::Image, "image/heif"),
    ("bmp", Kind::Image, "image/bmp"),
    ("tif", Kind::Image, "image/tiff"),
    ("tiff", Kind::Image, "image/tiff"),
    ("svg", Kind::Image, "image/svg+xml"),
    ("psd", Kind::Image, "image/vnd.adobe.photoshop"),
    // What a camera writes before anything has developed it. One entry per
    // manufacturer because that is how raw works — there is no shared
    // extension and no shared format — and they are here because a shelf of
    // photographs that stopped at the JPEGs would be missing the originals.
    ("dng", Kind::Image, "image/x-adobe-dng"),
    ("cr2", Kind::Image, "image/x-canon-cr2"),
    ("cr3", Kind::Image, "image/x-canon-cr3"),
    ("nef", Kind::Image, "image/x-nikon-nef"),
    ("arw", Kind::Image, "image/x-sony-arw"),
    ("orf", Kind::Image, "image/x-olympus-orf"),
    ("raf", Kind::Image, "image/x-fuji-raf"),
    ("rw2", Kind::Image, "image/x-panasonic-rw2"),
];

/// What a path is, by its extension alone: which shelf it belongs on and what
/// it would be handed to an application as.
///
/// The one place that question is answered, so the walk that finds a file, the
/// row that draws it and the worker that makes a picture of it cannot come to
/// different conclusions about what it is.
fn described(path: &Path) -> Option<(Kind, &'static str)> {
    let extension = path
        .extension()
        .and_then(OsStr::to_str)?
        .to_ascii_lowercase();
    TYPES
        .iter()
        .find(|(name, ..)| *name == extension)
        .map(|(_, kind, mime)| (*kind, *mime))
}

/// Whether a file of this type has a picture in it worth showing in place of
/// its mark.
///
/// The same question [`Kind::has_picture`] answers, asked of a type rather than
/// of a shelf: the file explorer meets files by extension and has no shelf to
/// ask. One rule underneath both, so a photograph is thumbnailed in a folder if
/// and only if it would have been on a shelf.
pub fn has_picture(mime: &str) -> bool {
    TYPES
        .iter()
        .any(|(_, kind, own)| *own == mime && kind.has_picture())
}

/// Which shelf a path belongs on.
pub fn kind_of(path: &Path) -> Option<Kind> {
    described(path).map(|(kind, _)| kind)
}

/// And what it would be handed to an application as, for the files the shelves
/// know about.
///
/// Asked from outside by the file explorer, which meets the same files in the
/// folders they are actually in and must not come to a different conclusion
/// about what they are — a `.flac` is `audio/flac` whichever column it is being
/// looked at from. `None` for everything the shelves do not hold, which
/// [`crate::files`] then answers for out of its own, wider table.
pub fn mime_of(path: &Path) -> Option<&'static str> {
    described(path).map(|(_, mime)| mime)
}

/// One file on one of the shelves.
#[derive(Debug, Clone)]
pub struct File {
    /// What the row is called: the file name without its extension. Not the
    /// title tag inside the file — reading those means opening every file the
    /// walk finds, which is the cost this whole module is arranged to avoid.
    pub title: String,
    /// The folder it was found in, with the home directory written as `~`.
    ///
    /// The line under the title, and the only thing that tells two files of
    /// the same name apart — which, in a music collection, is every track that
    /// appears on both an album and a compilation.
    pub folder: String,
    pub path: PathBuf,
    pub kind: Kind,
    /// What the file would be handed to an application as, and what [`Sort::Type`]
    /// groups the shelf by. Held rather than worked out from the extension each
    /// time: sorting twenty thousand rows asks every one of them this several
    /// times over, and it is a table lookup and a lowercased allocation.
    pub mime: &'static str,
    /// How much of the disk it takes, in bytes. Zero for a file the shell could
    /// not stat, which is the same thing the row does with it: an unknown size
    /// is the smallest one there is rather than a reason to leave the file out.
    pub size: u64,
    /// When it was made and when it was last written, as the filesystem
    /// records them.
    ///
    /// Both optional, and for different reasons. `modified` is on every Unix
    /// filesystem there is and is absent only if the file could not be
    /// examined at all; `created` is `statx`'s birth time, which several
    /// filesystems simply do not keep — so a home directory on one of them
    /// sorts by a date that is not there, and [`Sort::orders`] says so rather
    /// than inventing one.
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    /// The title folded to lower case, kept rather than worked out each time.
    /// The shelf is re-merged on every batch the walker sends, so this is
    /// compared thousands of times per pass and allocating for each comparison
    /// would be the most expensive thing the shell does.
    order: String,
}

impl File {
    /// The file at `path`, if it is one of the kinds the bar lists.
    ///
    /// `None` for anything else, and for a path that is not valid UTF-8:
    /// opening a file means handing its name to a command line, and a name
    /// that cannot be written down is one the shell could list but never play.
    /// Showing a row that does nothing is worse than not showing it.
    ///
    /// This is where the walk's one and only `stat` happens, and it happens
    /// once per file per session rather than once per pass — see [`sweep`].
    /// A file the shell cannot examine is still listed, with nothing said about
    /// its size or its age: it is on the disk, the user can see it there, and a
    /// row that disappeared because a permission bit made one syscall fail
    /// would be a stranger answer than a row that sorts to the end.
    pub fn at(path: &Path) -> Option<File> {
        let (kind, mime) = described(path)?;
        let title = path.file_stem().and_then(OsStr::to_str)?;
        path.to_str()?;

        // Not `metadata`: that follows links, and the walk deliberately does
        // not. What is being described is the entry that was found.
        let facts = std::fs::symlink_metadata(path).ok();
        Some(File {
            title: title.to_string(),
            folder: crate::screenshot::abbreviated(path.parent()?),
            path: path.to_path_buf(),
            kind,
            mime,
            size: facts.as_ref().map(|facts| facts.len()).unwrap_or_default(),
            created: facts.as_ref().and_then(|facts| facts.created().ok()),
            modified: facts.as_ref().and_then(|facts| facts.modified().ok()),
            order: title.to_lowercase(),
        })
    }

    /// Alphabetical, ignoring case, with the path breaking ties.
    ///
    /// The path rather than nothing, because two files with the same name in
    /// different folders are two rows and the order they come out in must not
    /// depend on which of them the walk reached first. It is also what settles
    /// every other order in this module: two files of the same size, or written
    /// in the same second, are put in the order the user already knows.
    fn order_by(a: &File, b: &File) -> std::cmp::Ordering {
        a.order.cmp(&b.order).then_with(|| a.path.cmp(&b.path))
    }
}

/// The order a shelf is listed in.
///
/// Nine of them, because they are the questions somebody actually asks of a
/// folder full of their own files: what is it called, how big is it, what kind
/// is it, and when did it happen. Each of the four that has a direction is two
/// orders rather than one order with a flag, for the reason
/// [`Command::MoveToNextDisplay`](crate::menu::Command::MoveToNextDisplay)
/// gives: what the user picks is a row with a name on it, and "biggest first"
/// and "smallest first" are two names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// The one the shelves are kept in, and the only one that costs nothing.
    #[default]
    NameAscending,
    NameDescending,
    LargestFirst,
    SmallestFirst,
    /// By what the file *is*, so every photograph of one format stands with
    /// the others. The mime type rather than the extension, which is what makes
    /// `.jpg` and `.jpeg` one group instead of two.
    Type,
    NewestFirst,
    OldestFirst,
    LastChangedFirst,
    LongestUntouchedFirst,
}

/// Every order, in the one place that decides what a Sort menu looks like: the
/// rows are built from this, so a new order is a variant and a line here.
pub const SORTS: &[Sort] = &[
    Sort::NameAscending,
    Sort::NameDescending,
    Sort::LargestFirst,
    Sort::SmallestFirst,
    Sort::Type,
    Sort::NewestFirst,
    Sort::OldestFirst,
    Sort::LastChangedFirst,
    Sort::LongestUntouchedFirst,
];

impl Sort {
    /// What the row that chooses it says.
    pub fn label(self) -> &'static str {
        match self {
            Sort::NameAscending => "Name (A to Z)",
            Sort::NameDescending => "Name (Z to A)",
            Sort::LargestFirst => "Size (largest first)",
            Sort::SmallestFirst => "Size (smallest first)",
            Sort::Type => "Type",
            Sort::NewestFirst => "Created (newest first)",
            Sort::OldestFirst => "Created (oldest first)",
            Sort::LastChangedFirst => "Modified (newest first)",
            Sort::LongestUntouchedFirst => "Modified (oldest first)",
        }
    }

    /// What it is called in the settings file.
    ///
    /// Written out rather than derived from the variant name, because it is a
    /// thing a user may open a text editor and read: the file is theirs, and
    /// `size-largest-first` says what it does where `LargestFirst` says what a
    /// Rust enum is called.
    pub fn key(self) -> &'static str {
        match self {
            Sort::NameAscending => "name",
            Sort::NameDescending => "name-reversed",
            Sort::LargestFirst => "size-largest-first",
            Sort::SmallestFirst => "size-smallest-first",
            Sort::Type => "type",
            Sort::NewestFirst => "created-newest-first",
            Sort::OldestFirst => "created-oldest-first",
            Sort::LastChangedFirst => "modified-newest-first",
            Sort::LongestUntouchedFirst => "modified-oldest-first",
        }
    }

    /// The order of that name, or `None` for a file that names one this shell
    /// does not have — a hand-edited typo, or a setting from a later version.
    pub fn from_key(key: &str) -> Option<Sort> {
        SORTS.iter().copied().find(|sort| sort.key() == key)
    }

    /// Whether a shelf that knows these things has anything to sort by in this
    /// order.
    ///
    /// False only where the filesystem keeps no such time at all — no birth
    /// time on this volume, most often — and the row is then drawn as an
    /// outline. An order that is offered and does nothing is worse than one
    /// that says out loud it cannot be had here.
    pub fn orders(self, knows: Orders) -> bool {
        match self {
            Sort::NewestFirst | Sort::OldestFirst => knows.created,
            Sort::LastChangedFirst | Sort::LongestUntouchedFirst => knows.modified,
            _ => true,
        }
    }

    /// Put `a` before `b`, or after it.
    fn compare(self, a: &File, b: &File) -> std::cmp::Ordering {
        let then = || File::order_by(a, b);
        match self {
            Sort::NameAscending => then(),
            Sort::NameDescending => File::order_by(b, a),
            Sort::LargestFirst => b.size.cmp(&a.size).then_with(then),
            Sort::SmallestFirst => a.size.cmp(&b.size).then_with(then),
            Sort::Type => a.mime.cmp(b.mime).then_with(then),
            Sort::NewestFirst => by_time(a.created, b.created, true).then_with(then),
            Sort::OldestFirst => by_time(a.created, b.created, false).then_with(then),
            Sort::LastChangedFirst => by_time(a.modified, b.modified, true).then_with(then),
            Sort::LongestUntouchedFirst => by_time(a.modified, b.modified, false).then_with(then),
        }
    }
}

/// Compare two times, with a file whose time is unknown always last — whichever
/// end of the list that is.
///
/// Shared with [`crate::files`], which puts a directory in these same nine
/// orders: this rule is the subtle half of them, and two copies of it would be
/// two chances to get it wrong.
///
/// Not `Option`'s own ordering, and not the same comparison read backwards.
/// `None` sorts before `Some`, so reversing the arguments to get "newest first"
/// would also move the files nothing is known about from one end of the shelf
/// to the other: they would be at the head of one order and the tail of its
/// reverse, which is not what reversing an order means. Unknown is last in
/// both, because it is not a date at all.
pub fn by_time(
    a: Option<SystemTime>,
    b: Option<SystemTime>,
    newest_first: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(a), Some(b)) if newest_first => b.cmp(&a),
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// One file on a shelf, as everything downstream of the walk holds it.
///
/// Shared rather than copied. The rows of the bar are a *copy* of the shelves —
/// see [`crate::apps::media_rows`] — and they are made again from scratch
/// every time anything changes, which during a walk is four times a second. A
/// collection of any size would otherwise have its every title, folder and path
/// duplicated and thrown away at that rate, which is both the memory and the
/// allocator twice over for a list nobody has scrolled to the end of. A file is
/// also never edited once found, so there is nothing for two owners to disagree
/// about.
pub type Shelved = Arc<File>;

/// The shelves themselves: what has been found, in what order it is listed.
///
/// Everything here costs something in proportion to the size of the
/// collection — merging a batch in, putting an order on, building the rows —
/// and none of it happens on the thread that draws. It lives on the worker,
/// behind [`Library`], which is the shell's end of the wire. See [`work`].
///
/// One walk fills all three: the expensive half of this is reading the
/// directories, the extension of what is in them decides which shelf a file
/// lands on, and a second pass over the same home directory to look for
/// pictures would cost everything the first one cost to find out something it
/// already knew.
struct Shelves {
    audio: Vec<Shelved>,
    video: Vec<Shelved>,
    images: Vec<Shelved>,
    /// What order each shelf is *listed* in, which is not the order it is
    /// *kept* in. See [`Shelves::listing`].
    sorts: [Sort; 3],
    /// What each shelf is being searched for, empty for one nobody has
    /// searched. As the user typed it; the folding happens once per listing.
    queries: [String; 3],
    /// Whether a pass has finished, so an empty shelf can say whether it is
    /// still looking or has looked.
    settled: bool,
}

impl Shelves {
    fn new(sorts: [Sort; 3]) -> Shelves {
        Shelves {
            audio: Vec::new(),
            video: Vec::new(),
            images: Vec::new(),
            sorts,
            queries: Default::default(),
            settled: false,
        }
    }

    /// Shelves holding exactly these files and walking nothing, so what the bar
    /// does with a shelf can be exercised without a home directory in it.
    #[cfg(test)]
    fn holding(files: Vec<Shelved>) -> Shelves {
        let mut shelves = Shelves::new([Sort::default(); 3]);
        shelves.settled = true;
        for file in files {
            shelves.file(vec![file]);
        }
        shelves
    }

    /// Put a batch of newly found files on the shelves they belong to, and say
    /// which of the three that changed.
    fn file(&mut self, batch: Vec<Shelved>) -> [bool; 3] {
        let mut audio = Vec::new();
        let mut video = Vec::new();
        let mut images = Vec::new();
        for found in batch {
            let shelf = match found.kind {
                Kind::Audio => &mut audio,
                Kind::Video => &mut video,
                Kind::Image => &mut images,
            };
            shelf.push(found);
        }
        let touched = [!audio.is_empty(), !video.is_empty(), !images.is_empty()];
        shelve(&mut self.audio, audio);
        shelve(&mut self.video, video);
        shelve(&mut self.images, images);
        touched
    }

    /// One shelf, as it is kept: alphabetical, whatever the user has asked the
    /// rows to be listed in.
    ///
    /// Kept in one order on purpose. The shelf is merged into on every batch
    /// the walk turns up, and a merge is only cheap because both sides are
    /// already in the same order. So the shelf is alphabetical always and the
    /// chosen order is put on afterwards, by [`Shelves::listing`].
    fn shelf(&self, kind: Kind) -> &[Shelved] {
        match kind {
            Kind::Audio => &self.audio,
            Kind::Video => &self.video,
            Kind::Image => &self.images,
        }
    }

    /// One shelf in the order its rows are drawn in, and holding only what the
    /// user is searching for.
    ///
    /// Shared handles rather than files: copying a collection's worth of
    /// titles and paths to sort them would be the copy [`Shelved`] exists to
    /// avoid. The one order the shelf is already in costs nothing beyond the
    /// list of handles itself.
    ///
    /// The search is applied before the order rather than after it, which is
    /// the whole reason it is worth doing here at all: a shelf of half a
    /// million photographs narrowed to nine is nine files to sort.
    fn listing(&self, kind: Kind) -> Vec<Shelved> {
        let mut listing: Vec<Shelved> = match self.searched(kind) {
            // Folded once per listing rather than once per file. The titles on
            // the other side of the comparison are folded already — see
            // [`File::order`] — so the whole of a search over a collection is
            // one allocation and a substring search per file.
            Some(needle) => self
                .shelf(kind)
                .iter()
                .filter(|file| file.order.contains(&needle))
                .cloned()
                .collect(),
            None => self.shelf(kind).to_vec(),
        };
        let sort = self.sort(kind);
        if sort != Sort::NameAscending {
            listing.sort_by(|a, b| sort.compare(a, b));
        }
        listing
    }

    /// What a shelf is being searched for, folded for comparison. `None` for a
    /// shelf nobody has searched, which is not the same as one searched for
    /// nothing: an empty search is every file, and this says so by never
    /// asking the question.
    fn searched(&self, kind: Kind) -> Option<String> {
        let query = self.query(kind);
        (!query.is_empty()).then(|| query.to_lowercase())
    }

    fn sort(&self, kind: Kind) -> Sort {
        self.sorts[shelf_index(kind)]
    }

    fn query(&self, kind: Kind) -> &str {
        &self.queries[shelf_index(kind)]
    }

    /// Search a shelf for something else. Returns whether that is a change —
    /// typing the query a shelf already holds is not news, and there is
    /// nothing to build for it.
    fn set_query(&mut self, kind: Kind, query: String) -> bool {
        if self.query(kind) == query {
            return false;
        }
        self.queries[shelf_index(kind)] = query;
        true
    }

    /// List a shelf in a different order. Returns whether that is a change —
    /// choosing the order a column is already in is not news, and there is
    /// nothing to build for it.
    fn set_sort(&mut self, kind: Kind, sort: Sort) -> bool {
        if self.sort(kind) == sort {
            return false;
        }
        self.sorts[shelf_index(kind)] = sort;
        true
    }

    /// Take a file off the shelves, because it is not on the disk any more.
    /// Says which shelf that was, if it was on one.
    fn forget(&mut self, path: &Path) -> Option<Kind> {
        for kind in [Kind::Audio, Kind::Video, Kind::Image] {
            let shelf = match kind {
                Kind::Audio => &mut self.audio,
                Kind::Video => &mut self.video,
                Kind::Image => &mut self.images,
            };
            let before = shelf.len();
            shelf.retain(|file| file.path != path);
            if shelf.len() != before {
                return Some(kind);
            }
        }
        None
    }

    /// The same for a folder that has gone, which is however many files at
    /// once and can be files on all three shelves.
    ///
    /// So it answers with all three rather than the one it found the file on:
    /// a folder somebody deletes is as likely as not to hold the photographs
    /// *and* the film of one afternoon.
    fn forget_below(&mut self, folder: &Path) -> [bool; 3] {
        let mut dirtied = [false; 3];
        for kind in [Kind::Audio, Kind::Video, Kind::Image] {
            let shelf = match kind {
                Kind::Audio => &mut self.audio,
                Kind::Video => &mut self.video,
                Kind::Image => &mut self.images,
            };
            let before = shelf.len();
            // Whole components, which is what `Path::starts_with` compares:
            // `~/Music/Live` is not a prefix of `~/Music/Liverpool`, and a
            // rule written on the strings would have emptied that shelf too.
            shelf.retain(|file| !file.path.starts_with(folder));
            dirtied[shelf_index(kind)] = shelf.len() != before;
        }
        dirtied
    }

    /// One shelf, ready to hang on the bar.
    fn made(&self, kind: Kind) -> Made {
        let shelf = self.shelf(kind);
        let found = shelf.len();
        let query = self.query(kind);
        let listing = self.listing(kind);
        let matched = listing.len();
        Made {
            kind,
            rows: crate::apps::media_rows(listing, kind, query, found),
            // What the row the shelf hangs under says. A searched shelf says
            // so from *outside* the column as well: a list that has been cut
            // to nine of half a million photographs must not look like a
            // machine that has only nine.
            note: if query.is_empty() {
                note(kind, found, self.settled)
            } else {
                search_note(kind.plural(), matched, found)
            },
            orders: Orders {
                created: shelf.iter().any(|file| file.created.is_some()),
                modified: shelf.iter().any(|file| file.modified.is_some()),
            },
        }
    }
}

/// The three shelves these files make, as the worker would hand them over — so
/// what the bar does with a shelf can be exercised without a home directory to
/// walk or a thread to walk it on.
#[cfg(test)]
pub fn made_from(files: Vec<Shelved>) -> Vec<Made> {
    let shelves = Shelves::holding(files);
    [Kind::Audio, Kind::Video, Kind::Image]
        .into_iter()
        .map(|kind| shelves.made(kind))
        .collect()
}

/// One shelf as the bar takes it: the rows themselves, already in order, and
/// the two things the menus over them have to know.
///
/// Rows rather than files. Turning half a million files into half a million
/// rows means one allocation the size of the collection and a write per file,
/// and doing that on the shell's own thread is what the walk used to cost it:
/// a quarter of a second of dropped frames, four times a second, for as long
/// as the first pass lasted. So the rows are built where the files already
/// are, and what crosses back is finished work.
pub struct Made {
    pub kind: Kind,
    pub rows: Vec<crate::apps::Entry>,
    /// The line under the row the shelf hangs on.
    pub note: String,
    pub orders: Orders,
}

/// Which of the orders a shelf could actually be put in.
///
/// Worked out where the files are and carried across, because the question is
/// asked of a whole shelf — see [`Sort::orders`] — and the shell has no shelf
/// to ask any more. Two flags rather than a flag per order, since the four
/// orders that can be impossible are two questions about the filesystem.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Orders {
    /// Whether anything on the shelf has a creation time. Several filesystems
    /// keep none at all.
    pub created: bool,
    pub modified: bool,
}

/// What the shell asks of the worker.
///
/// Everything that changes a shelf, rather than everything the shell wants to
/// know: the answers all come back the same way, as rows.
enum Ask {
    /// List this shelf in a different order.
    Sort(Kind, Sort),
    /// List only the files on this shelf whose names hold this.
    ///
    /// One of these per keystroke while somebody is typing into the field at
    /// the head of a column, which is what the whole of this module's
    /// arrangement is for: narrowing a shelf is work in proportion to the
    /// collection, and it happens here rather than between two frames.
    Search(Kind, String),
    /// This file is not on the disk any more — the user has just deleted it.
    Forget(PathBuf),
    /// This file is on the disk now — the session has just written it.
    ///
    /// The exact opposite of [`Ask::Forget`], and there for the same reason:
    /// the walk would find it within [`AGAIN`], and a picture the user watched
    /// themselves take must not be missing from their own shelf for five
    /// minutes.
    Found(PathBuf),
    /// A whole folder has gone to the trash, so everything that was under it
    /// is off the shelves.
    ///
    /// Its own question rather than [`Ask::Forget`] arriving once per file,
    /// because the shell asking would have to walk the folder to know what to
    /// ask about — and it has just been renamed into the trash, so the walk
    /// would be of somewhere else. The shelves already hold every path they
    /// know; the prefix is the whole of what has to cross.
    ForgetBelow(PathBuf),
    /// Somebody has opened a shelf, so bring the next pass forward.
    ///
    /// Carries no kind. One walk answers for all three shelves — it is the
    /// reading of the directories that costs, and the extension of what is in
    /// them decides where a file lands — so "look again" is one question
    /// however it was asked.
    LookAgain,
    /// Rows the bar has finished with.
    ///
    /// Sent back to be dropped here rather than on the frame that replaced
    /// them. Letting go of half a million rows is half a million reference
    /// counts, which is small beside building them and still more than a
    /// frame has to spare.
    Discard(Vec<crate::apps::Entry>),
}

/// The shell's end of the wire.
///
/// Holds no files. What it has is the two channels, and an echo of the few
/// facts the menus ask about between one delivery and the next — which order
/// each shelf is in, and which orders it could be put in.
pub struct Library {
    asks: Option<mpsc::Sender<Ask>>,
    made: Option<Receiver<Made>>,
    sorts: [Sort; 3],
    /// What each shelf was last asked to be searched for.
    ///
    /// The shell's own record of what it asked, not a report of what has been
    /// done — the same distinction [`Self::set_sort`] draws, and it matters
    /// more here: the rows for one keystroke are still being built when the
    /// next one is typed, so the query on the bar is always a little ahead of
    /// the query the rows were made from.
    queries: [String; 3],
    orders: [Orders; 3],
}

impl Library {
    /// Start walking the user's home directory.
    ///
    /// A library with no walk behind it when there is no home directory to
    /// walk — the shelves are then empty and settled, and the rows say so.
    pub fn start() -> Library {
        // The orders the user last chose, read before the walk starts rather
        // than applied to the rows later, so that the first batch to arrive is
        // already in the right order — a column that came up alphabetical and
        // rearranged itself a quarter of a second in would be the shell
        // disagreeing with itself in front of the user.
        let mut sorts = [Sort::default(); 3];
        for kind in [Kind::Audio, Kind::Video, Kind::Image] {
            if let Some(sort) = crate::settings::media_sort(kind) {
                sorts[shelf_index(kind)] = sort;
            }
        }

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        let Some(home) = home else {
            tracing::info!("no home directory; the file rows will list nothing");
            return Library::settled();
        };

        let (send_made, made) = mpsc::channel();
        let (asks, take_asks) = mpsc::channel();
        let pace = Pace::default();
        std::thread::spawn(move || work(&home, sorts, pace, &send_made, &take_asks));
        Library {
            asks: Some(asks),
            made: Some(made),
            sorts,
            queries: Default::default(),
            orders: [Orders::default(); 3],
        }
    }

    /// A library that will never find anything: no home to walk, or a test.
    pub fn settled() -> Library {
        Library {
            asks: None,
            made: None,
            sorts: [Sort::default(); 3],
            queries: Default::default(),
            orders: [Orders::default(); 3],
        }
    }

    /// Every shelf the worker has finished since the last look.
    ///
    /// The whole of what the shell does with the library on an ordinary frame,
    /// and on nearly every frame it is an empty vector and a single failed
    /// `try_recv`.
    pub fn take(&mut self) -> Vec<Made> {
        let mut ready: Vec<Made> = Vec::new();
        let mut stale: Vec<Vec<crate::apps::Entry>> = Vec::new();
        while let Some(made) = self.made.as_ref() {
            match made.try_recv() {
                Ok(made) => {
                    self.orders[shelf_index(made.kind)] = made.orders;
                    // Only the last of any kind is worth hanging: a delivery is
                    // a whole shelf rather than a change to one, so an earlier
                    // one is a list that has already been superseded. Its rows
                    // go back to the worker with the rest — they are as
                    // expensive to let go of as the ones on the bar.
                    match ready.iter().position(|held| held.kind == made.kind) {
                        Some(at) => stale.push(std::mem::replace(&mut ready[at], made).rows),
                        None => ready.push(made),
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The worker only stops if it cannot deliver, which it cannot
                // do while this end is held.
                Err(TryRecvError::Disconnected) => {
                    self.made = None;
                    self.asks = None;
                }
            }
        }
        for rows in stale {
            self.discard(rows);
        }
        ready
    }

    /// What order a shelf is listed in.
    pub fn sort(&self, kind: Kind) -> Sort {
        self.sorts[shelf_index(kind)]
    }

    /// Which orders it can be put in at all.
    pub fn orders(&self, kind: Kind) -> Orders {
        self.orders[shelf_index(kind)]
    }

    /// List a shelf in a different order. Returns whether that is a change —
    /// choosing the order a column is already in is not news, and the bar has
    /// nothing to redraw for it.
    ///
    /// The rows themselves come back when the worker has built them, which for
    /// a large shelf is a moment later. The tick moves at once, because that is
    /// the shell's own record of what was asked for rather than a report of
    /// what has been done.
    pub fn set_sort(&mut self, kind: Kind, sort: Sort) -> bool {
        if self.sort(kind) == sort {
            return false;
        }
        self.sorts[shelf_index(kind)] = sort;
        self.ask(Ask::Sort(kind, sort));
        true
    }

    /// What a shelf is being searched for.
    pub fn search(&self, kind: Kind) -> &str {
        &self.queries[shelf_index(kind)]
    }

    /// Show only the files on a shelf whose names hold `query`, or all of them
    /// again for an empty one. Returns whether that is a change.
    ///
    /// Called on every keystroke, which is why it answers that question rather
    /// than the caller: a search field is a place where the same query is
    /// arrived at twice — a letter typed and taken back — and each of those is
    /// a shelf-sized rebuild that nobody would see the result of.
    ///
    /// The rows come back when the worker has narrowed the shelf and built
    /// them, which for a large one is a moment later. The field itself does not
    /// wait: it is the shell's own record of what has been typed, and a
    /// keyboard whose letters appeared a quarter of a second after they were
    /// pressed would be a keyboard nobody could type on.
    pub fn set_search(&mut self, kind: Kind, query: &str) -> bool {
        if self.search(kind) == query {
            return false;
        }
        self.queries[shelf_index(kind)] = query.to_string();
        self.ask(Ask::Search(kind, query.to_string()));
        true
    }

    /// Take a file off the shelves, because the user has just deleted it.
    ///
    /// The walk would find that out by itself within [`AGAIN`], which is far
    /// too long to leave a row standing for a file somebody has watched
    /// themselves delete. The row goes from the bar in the same breath — see
    /// [`crate::apps::forget_media`] — so what this does is keep the shelf it
    /// was built from honest.
    pub fn forget(&mut self, path: &Path) {
        self.ask(Ask::Forget(path.to_path_buf()));
    }

    /// Put a file on the shelves now, because the session has just written it.
    ///
    /// The screenshot the user has this second taken, and anything else this
    /// shell comes to make: it knows the path, so the shelf can know it too
    /// without anybody walking a home directory to rediscover a file that was
    /// named in the request that created it.
    ///
    /// Nothing is checked here. What the path *is* — a picture, a film, or
    /// something with no shelf at all — is the worker's question, answered the
    /// same way it answers it for the walk, so the two cannot disagree about a
    /// file they both found.
    pub fn found(&mut self, path: &Path) {
        self.ask(Ask::Found(path.to_path_buf()));
    }

    /// Take everything under `folder` off the shelves, because the folder has
    /// just gone to the trash.
    pub fn forget_below(&mut self, folder: &Path) {
        self.ask(Ask::ForgetBelow(folder.to_path_buf()));
    }

    /// Somebody has stepped into a shelf: look at the disk again soon.
    ///
    /// Cheap to call and cheap to call often. It does not start a scan — it
    /// moves the next pass forward to at most [`SOON`] after the last one
    /// finished — so a column stepped into ten times in a minute is still one
    /// walk of the home directory.
    pub fn look_again(&mut self) {
        self.ask(Ask::LookAgain);
    }

    /// Hand a list of rows back to be let go of on the worker.
    pub fn discard(&mut self, rows: Vec<crate::apps::Entry>) {
        if rows.is_empty() {
            return;
        }
        self.ask(Ask::Discard(rows));
    }

    fn ask(&mut self, ask: Ask) {
        if self
            .asks
            .as_ref()
            .is_some_and(|asks| asks.send(ask).is_err())
        {
            // The worker has gone. Nothing else will arrive, and nothing else
            // will be asked.
            self.asks = None;
            self.made = None;
        }
    }
}

/// Which of the three shelves a kind is, as an index — the one place the
/// enum's order and the array's order are tied together.
fn shelf_index(kind: Kind) -> usize {
    match kind {
        Kind::Audio => 0,
        Kind::Video => 1,
        Kind::Image => 2,
    }
}

/// What a shelf holding `found` files has to say for itself.
///
/// Three different things, because they are three different situations and only
/// one of them is "here is what there is". A row that said "No audio files"
/// while the walk was still in its first minute would be telling the user
/// something untrue about their own disk.
pub fn note(kind: Kind, found: usize, settled: bool) -> String {
    let what = if found == 1 {
        kind.singular()
    } else {
        kind.plural()
    };
    match (found, settled) {
        (0, false) => "Looking through your home folder".to_string(),
        (0, true) => format!("No {} in your home folder", kind.plural()),
        (_, false) => format!("{found} {what} so far"),
        (_, true) => format!("{found} {what} in your home folder"),
    }
}

/// What a shelf narrowed to a search has to say for itself.
///
/// Both numbers, always, because the pair is the whole of what the user needs
/// to know: how much they are being shown, and how much of their own
/// collection is standing behind it. A row saying only "9 photographs" over a
/// column of nine would be a shell claiming the other quarter million are not
/// there.
///
/// Whether the walk has settled is deliberately not asked. It is a question
/// about the *disk* — "there is nothing here" against "I have not looked yet" —
/// and a search is a question about the shelf as it stands, which is answered
/// the same way whether or not the walk has more to find. What is still coming
/// arrives in this column exactly as it arrives in an unsearched one.
pub fn search_note(plural: &str, matched: usize, found: usize) -> String {
    let of_all = format!("of {found} {plural}");
    match matched {
        0 => format!("No {plural} match"),
        // The verb has to agree with the count, and the count is the user's
        // rather than ours: a collection with exactly one Beatles track in it
        // is a common enough answer to be worth writing the sentence for.
        1 => format!("1 {of_all} matches"),
        _ => format!("{matched} {of_all} match"),
    }
}

/// Merge a batch of newly found files into a shelf that is already in order.
///
/// A merge rather than a push and a sort: the shelf is sorted already and the
/// batch is small beside it, so this is one pass over both instead of a
/// re-sort of everything on every quarter second of the walk.
fn shelve(shelf: &mut Vec<Shelved>, mut batch: Vec<Shelved>) {
    if batch.is_empty() {
        return;
    }
    batch.sort_by(|a, b| File::order_by(a, b));

    let mut merged = Vec::with_capacity(shelf.len() + batch.len());
    let mut old = std::mem::take(shelf).into_iter().peekable();
    let mut new = batch.into_iter().peekable();
    loop {
        let take_new = match (old.peek(), new.peek()) {
            (Some(a), Some(b)) => File::order_by(b, a) == std::cmp::Ordering::Less,
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => break,
        };
        merged.push(if take_new {
            new.next().expect("peeked")
        } else {
            old.next().expect("peeked")
        });
    }
    *shelf = merged;
}

/// Every file the walk has told the shell about, and the pass it last saw each
/// of them on.
///
/// One map rather than the obvious pair of sets — everything reported, and
/// everything seen this time round, diffed at the end. The pair holds a second
/// copy of every path in the collection for the length of a pass, which on a
/// home directory with half a million pictures in it is tens of megabytes of
/// nothing but duplicate paths. A pass number costs four bytes against a path's
/// sixty, and the files that have gone are the ones whose number did not move.
type Reported = std::collections::HashMap<PathBuf, u64>;

/// The worker: walk the home directory over and over, keep the shelves, and
/// hand the bar finished rows.
///
/// One thread for both halves rather than a walker feeding a shelver. The two
/// are the same job on the same files — what is found is merged in, ordered
/// and built into rows, and none of it is wanted anywhere else — and a second
/// thread would buy nothing but a channel to copy every file through.
fn work(
    home: &Path,
    sorts: [Sort; 3],
    pace: Pace,
    made: &mpsc::Sender<Made>,
    asks: &Receiver<Ask>,
) {
    let mut worker = Worker {
        shelves: Shelves::new(sorts),
        pending: Vec::new(),
        dirty: [false; 3],
        published: Instant::now(),
        built: Duration::ZERO,
        reported: Reported::new(),
        pass: 0,
        made,
        asks,
    };

    for pass in 0.. {
        worker.pass = pass;
        let started = Instant::now();
        if sweep(home, 0, &mut worker).is_err() {
            return;
        }

        let mut gone: Vec<PathBuf> = Vec::new();
        worker.reported.retain(|path, seen| {
            if *seen == pass {
                return true;
            }
            gone.push(path.clone());
            false
        });
        for path in &gone {
            if let Some(kind) = worker.shelves.forget(path) {
                worker.dirty[shelf_index(kind)] = true;
            }
        }
        tracing::debug!(
            files = worker.reported.len(),
            gone = gone.len(),
            took = started.elapsed().as_secs_f32(),
            "walked the home folder for music, video and pictures"
        );

        // The end of a pass is news whatever it turned up: until it has
        // happened an empty shelf means "still looking" rather than "there is
        // nothing here", and that sentence is on the bar.
        let first = !worker.shelves.settled;
        worker.shelves.settled = true;
        if first {
            worker.dirty = [true; 3];
        }
        if worker.publish().is_err() {
            return;
        }
        if worker.rest(pace).is_err() {
            return;
        }
    }
}

/// How often the walk comes round, and how soon somebody stepping into a shelf
/// may bring it forward.
///
/// A pair rather than two constants read where they are used, because a test
/// that had to wait out a real [`AGAIN`] to prove anything about the second
/// number would be a test nobody runs.
#[derive(Debug, Clone, Copy)]
struct Pace {
    again: Duration,
    soon: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Pace {
            again: AGAIN,
            soon: SOON,
        }
    }
}

/// How long to leave between one delivery and the next, given what the last
/// one cost to build.
///
/// [`PUBLISH`] until the shelves are large enough that building their rows is
/// itself expensive, and then a multiple of that — so no more of the worker's
/// time goes into rebuilding lists than into reading the directories they came
/// from.
///
/// Without it the tail of a walk is the worst part of it: a file turning up
/// every quarter second in some slow corner of the home directory would rebuild
/// half a million rows for each one, over and over, for a change nobody could
/// see. The shell would not drop a frame for it — that is what the thread is
/// for — but the machine would still be doing it. On the home directory this
/// was written against it is the difference between eight deliveries and
/// fifty-seven.
fn wait_after(built: Duration) -> Duration {
    /// How much of the worker's time may go into publishing: one part in this
    /// many.
    const SPARE: u32 = 4;
    PUBLISH.max(built * SPARE)
}

/// The worker's own state: the shelves, and what it has not handed over yet.
struct Worker<'a> {
    shelves: Shelves,
    /// Found since the last publishing and not on a shelf yet.
    ///
    /// Merging is one pass over the whole shelf, so it happens once per
    /// [`PUBLISH`] with everything that arrived in between rather than once per
    /// file. On a home directory with half a million pictures in it that is the
    /// difference between a walk that costs a few seconds and one that costs
    /// minutes.
    pending: Vec<Shelved>,
    /// Which shelves have something the bar has not been told about.
    dirty: [bool; 3],
    published: Instant,
    /// What the last delivery cost to build — see [`Worker::wait`].
    built: Duration,
    /// Every file the shelves know about, and when each was last seen.
    ///
    /// Held by the worker rather than by the walk because the walk is no
    /// longer the only thing that finds a file: a screenshot handed over by
    /// [`Ask::Found`] has to be written down here too, or the next pass would
    /// meet a file it has never heard of and shelve a second row for it.
    reported: Reported,
    /// Which pass is running, or has most recently run.
    pass: u64,
    made: &'a mpsc::Sender<Made>,
    asks: &'a Receiver<Ask>,
}

impl Worker<'_> {
    /// A file the walk has just turned up, and had not seen before. Hands over
    /// what is ready, if it is time to.
    fn found(&mut self, path: PathBuf, file: File) -> Result<(), Gone> {
        self.reported.insert(path, self.pass);
        self.pending.push(Arc::new(file));
        if self.published.elapsed() < wait_after(self.built) {
            return Ok(());
        }
        self.publish()
    }

    /// Merge in what has arrived, build the rows of every shelf that has
    /// changed, and send them.
    fn publish(&mut self) -> Result<(), Gone> {
        let started = Instant::now();
        self.listen();
        if !self.pending.is_empty() {
            let batch = std::mem::take(&mut self.pending);
            let touched = self.shelves.file(batch);
            for (dirty, touched) in self.dirty.iter_mut().zip(touched) {
                *dirty |= touched;
            }
        }
        for kind in [Kind::Audio, Kind::Video, Kind::Image] {
            if !std::mem::take(&mut self.dirty[shelf_index(kind)]) {
                continue;
            }
            self.made.send(self.shelves.made(kind)).map_err(|_| Gone)?;
        }
        self.built = started.elapsed();
        self.published = Instant::now();
        Ok(())
    }

    /// Everything the shell has asked for since the last look.
    fn listen(&mut self) {
        while let Ok(ask) = self.asks.try_recv() {
            self.take(ask);
        }
    }

    fn take(&mut self, ask: Ask) {
        match ask {
            Ask::Sort(kind, sort) => {
                if self.shelves.set_sort(kind, sort) {
                    self.dirty[shelf_index(kind)] = true;
                }
            }
            Ask::Search(kind, query) => {
                if self.shelves.set_query(kind, query) {
                    self.dirty[shelf_index(kind)] = true;
                }
            }
            Ask::Forget(path) => {
                self.reported.remove(&path);
                if let Some(kind) = self.shelves.forget(&path) {
                    self.dirty[shelf_index(kind)] = true;
                }
            }
            Ask::ForgetBelow(folder) => {
                // What has been reported goes as well as what is shelved, or
                // the next pass would find nothing changed under a folder that
                // is no longer there and leave the shelves as they are.
                self.reported.retain(|path, _| !path.starts_with(&folder));
                for (dirty, gone) in self
                    .dirty
                    .iter_mut()
                    .zip(self.shelves.forget_below(&folder))
                {
                    *dirty |= gone;
                }
            }
            Ask::Found(path) => {
                // Already on a shelf: the walk reached it first, or the same
                // file has been handed over twice. Either way there is one
                // file and it gets one row.
                if self.reported.contains_key(&path) {
                    return;
                }
                let Some(file) = File::at(&path) else {
                    // Not a kind the bar lists, or not a file at all. Said out
                    // loud at debug because the caller believed it had just
                    // written one.
                    tracing::debug!(path = %path.display(), "nothing to shelve here");
                    return;
                };
                let kind = file.kind;
                self.reported.insert(path, self.pass);
                self.pending.push(Arc::new(file));
                self.dirty[shelf_index(kind)] = true;
            }
            // Answered by [`Worker::rest`], which is where waiting happens and
            // so the only place that can stop doing it.
            Ask::LookAgain => {}
            // Dropped here, on the way out of this function, which is the
            // whole point of it having been sent.
            Ask::Discard(rows) => drop(rows),
        }
    }

    /// Wait for the next pass, answering the shell in the meantime.
    ///
    /// Not a sleep. Between passes is where this thread spends nearly all of
    /// its life, and a shelf asked for in a different order five seconds after
    /// the walk finished must not wait out the rest of five minutes for it.
    ///
    /// Nor a fixed wait. Somebody stepping into a shelf cuts it short — to
    /// `pace.soon` after the last pass ended, which is now for a rest that has
    /// been going longer than that. The floor is the whole of the rate limit:
    /// the wait can only ever be shortened to it, so any number of people
    /// asking any number of times is still one walk.
    fn rest(&mut self, pace: Pace) -> Result<(), Gone> {
        let began = Instant::now();
        let mut until = began + pace.again;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            match self.asks.recv_timeout(left) {
                Ok(ask) => {
                    if matches!(ask, Ask::LookAgain) {
                        until = until.min(began + pace.soon);
                    }
                    self.take(ask);
                    if self.dirty.iter().any(|dirty| *dirty) {
                        self.publish()?;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
                // The shell has gone.
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Gone),
            }
        }
    }
}

/// Nobody is listening any more.
///
/// Its own type rather than the channel's `SendError`, which carries the
/// undelivered report — a whole `File` — back up through every level of the
/// walk's recursion as an error value. What the walk needs to know is that
/// there is no shell on the other end, and that is one bit.
struct Gone;

/// One directory, and everything under it. `Err` once nobody is listening,
/// which is the only thing that stops a pass early.
fn sweep(dir: &Path, depth: usize, worker: &mut Worker) -> Result<(), Gone> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let pass = worker.pass;

    for entry in entries.flatten() {
        let name = entry.file_name();
        // Hidden folders are caches, state and application data — every
        // desktop's indexer skips them, and the sound effects buried in a
        // game's private directory are nobody's music. A hidden *file* is
        // skipped for the same reason: it is not something the user filed.
        if name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();

        if kind.is_dir() {
            // Not `is_dir` on the metadata: that follows links, and a link
            // pointing at its own ancestor is a walk that never ends. A user
            // who has linked a music drive into their home is served by the
            // re-walk finding nothing there rather than by the shell hanging.
            if depth < DEPTH {
                sweep(&path, depth + 1, worker)?;
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }

        // The extension first, because it is free — it is already in hand from
        // the directory entry — and it rules out all but a fraction of a home
        // directory. Only what survives it is worth a syscall.
        if kind_of(&path).is_none() {
            continue;
        }
        // A file that has been reported before is examined no further. Every
        // pass after the first would otherwise `stat` the whole collection
        // again to learn what it already knows, and the size and the dates of
        // a file are read once, when it is first found. A file rewritten in
        // place during a session therefore keeps the dates it was found with
        // until the shell is started again — which is the cost of not holding
        // an open watch on every directory under `$HOME`.
        if let Some(seen) = worker.reported.get_mut(&path) {
            *seen = pass;
            continue;
        }
        let Some(file) = File::at(&path) else {
            continue;
        };
        worker.found(path, file)?;
    }
    Ok(())
}

/// What would open a file, and what to call it while it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    /// The application, for the log — the splash is about the file.
    pub name: String,
    /// Its picture by icon name, for the row that offers it in an Open with
    /// list. Nothing for `xdg-open`, which is not an application the user
    /// installed and has no icon of its own.
    pub icon: Option<String>,
    /// A command line, ready for [`crate::model::launch`].
    pub command: String,
}

/// What to open `file` with.
///
/// The user has already answered this question for their desktop, so it is
/// read rather than asked again: the default they set for the type, then any
/// installed application that says it handles the type, then `xdg-open` — and
/// nothing at all if none of the three has an answer, because a row that
/// starts the wrong program is worse than one that reports it could not start
/// anything.
///
/// The catalogue the shell already scanned is what the first two are looked up
/// in. It knows every installed application's `Exec` and `MimeType`, which is
/// the whole of what a handler lookup needs, and it means the process that
/// gets started is one the shell can name, count and close like any other.
/// A path and a type rather than a file, because that is everything the answer
/// depends on and there are two kinds of row that have them: a file the walk
/// shelved, and a file the explorer found in the folder it lives in. One
/// function, so a `.flac` opens in the same application whichever column it was
/// pressed in.
pub fn opening(path: &Path, mime: &str, categories: &[Category]) -> Option<Opening> {
    if let Some(app) = handlers(mime, categories).first() {
        return Some(opening_with(path, app));
    }

    if crate::model::executable_on_path(OsStr::new("xdg-open")) {
        return Some(Opening {
            name: "xdg-open".to_string(),
            icon: None,
            command: format!("xdg-open {}", quoted(path)),
        });
    }

    tracing::warn!(
        mime,
        file = %path.display(),
        "nothing installed opens this, and there is no xdg-open to ask"
    );
    None
}

/// Everything installed that will open `file`, best first.
///
/// What [`opening`] picks from, and what the Open with menu lists — one
/// function, so the first row of that menu is always the application a plain
/// Open would have used. Anything else would make Open with a way of finding
/// out that Open does something else.
///
/// The user's own default heads the list where they have set one, and the rest
/// follow in [`declares`]'s order. The default is not repeated further down: it
/// is one application and it gets one row.
pub fn handlers<'a>(mime: &str, categories: &'a [Category]) -> Vec<&'a App> {
    let chosen = preferred(mime, categories);
    let mut handlers: Vec<&App> = chosen.into_iter().collect();
    handlers.extend(
        ranked(mime, categories)
            .into_iter()
            .filter(|app| chosen.is_none_or(|chosen| !std::ptr::eq(chosen, *app))),
    );
    handlers
}

/// What starting the file at `path` in `app` would take.
pub fn opening_with(path: &Path, app: &App) -> Opening {
    Opening {
        name: app.name.clone(),
        icon: app.icon.clone(),
        command: with_file(&app.exec, path),
    }
}

/// One application on the Open with list: what the row says, and what choosing
/// it would write down.
///
/// Not an [`Opening`], because choosing one of these starts nothing. The list
/// answers "which program opens these", and the answer outlives the file it was
/// asked over — so what a row has to carry is the desktop entry's own name,
/// which is how a `mimeapps.list` names an application, and the type the
/// answer is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handler {
    pub name: String,
    /// Its picture by icon name, for the row that offers it.
    pub icon: Option<String>,
    /// The desktop entry's file name — `vlc.desktop` and the like.
    pub id: String,
    /// The type this application would be made the default for.
    pub mime: &'static str,
}

/// The row `app` gets on the Open with list for a file of this kind.
///
/// `None` for an application whose desktop entry has no file name worth
/// writing down, which cannot happen for anything the catalogue scanned off
/// the disk — but a row that could be chosen and then not recorded would be a
/// row that lies about what it did.
pub fn handler_row(mime: &'static str, app: &App) -> Option<Handler> {
    Some(Handler {
        name: app.name.clone(),
        icon: app.icon.clone(),
        id: app.path.file_name().and_then(OsStr::to_str)?.to_string(),
        mime,
    })
}

/// Write `handler` down as the application that opens its type from now on.
///
/// The user's own `mimeapps.list`, which is where every other desktop keeps
/// this and where [`preferred`] reads it back from — so the choice holds for
/// the file manager and the browser as much as for this shell, and it holds
/// after a reboot. Only the `[Default Applications]` group is touched: the rest
/// of the file is the user's, and a shell that rewrote it wholesale would drop
/// whatever it did not understand.
///
/// Returns whether it was written.
pub fn make_default(handler: &Handler) -> bool {
    let Some(path) = default_list_path() else {
        tracing::warn!("no config directory to record the default application in");
        return false;
    };
    let Some(directory) = path.parent() else {
        return false;
    };
    if let Err(err) = std::fs::create_dir_all(directory) {
        tracing::warn!(%err, path = %directory.display(), "could not create the config directory");
        return false;
    }

    // Missing is the ordinary case on a machine whose owner has never chosen a
    // default for anything, and an empty file is what that means.
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let body = with_default(&raw, handler.mime, &handler.id);

    // Through a temporary and a rename, the way the shell's own settings are
    // written: this file is read by everything on the machine, and a
    // half-written one would leave the user with no defaults at all.
    let temporary = path.with_extension("list.new");
    if let Err(err) = std::fs::write(&temporary, body) {
        tracing::warn!(%err, path = %temporary.display(), "could not write the default applications");
        return false;
    }
    if let Err(err) = std::fs::rename(&temporary, &path) {
        tracing::warn!(%err, path = %path.display(), "could not replace the default applications");
        let _ = std::fs::remove_file(&temporary);
        return false;
    }
    true
}

/// The `mimeapps.list` a choice is written to: the user's own, under their
/// config directory.
///
/// The first of [`mimeapps_files`] is the same file, but only when it exists —
/// this is the one place that has to name it whether or not it does.
fn default_list_path() -> Option<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(config) if config.is_absolute() => config,
        _ => std::env::var_os("HOME").map(PathBuf::from)?.join(".config"),
    };
    Some(config.join("mimeapps.list"))
}

/// A `mimeapps.list` with `mime` set to open in `id`, and everything else in it
/// left exactly as it was.
///
/// Three cases, and the file decides which: the type already has a default and
/// that line is replaced, the group is there and the line is added to the end
/// of it, or there is no group at all and one is added. The last is what a
/// machine whose owner has never chosen a default looks like.
fn with_default(raw: &str, mime: &str, id: &str) -> String {
    let setting = format!("{mime}={id}");
    let mut lines: Vec<String> = raw.lines().map(str::to_string).collect();

    let group = lines
        .iter()
        .position(|line| line.trim() == "[Default Applications]");
    let Some(group) = group else {
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("[Default Applications]".to_string());
        lines.push(setting);
        lines.push(String::new());
        return lines.join("\n");
    };

    // The group runs to the next one, or to the end of the file.
    let end = lines
        .iter()
        .skip(group + 1)
        .position(|line| line.trim().starts_with('['))
        .map_or(lines.len(), |at| group + 1 + at);

    let existing = lines[group + 1..end].iter().position(|line| {
        line.split_once('=')
            .is_some_and(|(key, _)| key.trim() == mime)
    });
    match existing {
        Some(at) => lines[group + 1 + at] = setting,
        // Above whatever blank lines separate this group from the next, so the
        // file keeps the shape it had.
        None => {
            let at = lines[group + 1..end]
                .iter()
                .rposition(|line| !line.trim().is_empty())
                .map_or(group + 1, |at| group + 2 + at);
            lines.insert(at, setting);
        }
    }
    let mut body = lines.join("\n");
    body.push('\n');
    body
}

/// An `Exec` line with the file to open put where that application expects it.
///
/// On the end, for almost everything: the field codes are already stripped and
/// what is left is the program and its own arguments.
///
/// The exception is flatpak's file forwarding, which is why this is not one
/// `format!`. A flatpak entry runs `flatpak run --file-forwarding APP @@u %u
/// @@`, and the pair of `@@` markers is where the files go — flatpak exports
/// what is *between* them into the sandbox and rewrites the path the
/// application is handed. A file appended after the closing marker is passed
/// through untouched, and the application then opens a path that does not
/// exist inside its own filesystem. So it goes inside the markers, which is
/// what they are for.
fn with_file(exec: &str, path: &Path) -> String {
    let file = quoted(path);
    let mut argv: Vec<&str> = exec.split_whitespace().collect();
    match argv.iter().rposition(|word| *word == "@@") {
        Some(closing) => {
            argv.insert(closing, &file);
            argv.join(" ")
        }
        None => format!("{exec} {file}"),
    }
}

/// A path as a `sh -c` command line can carry it.
///
/// Single quotes, because inside them a shell expands nothing at all — and the
/// one character that cannot appear in them is closed, escaped and reopened.
/// Music is full of apostrophes, brackets, dollar signs and spaces, and every
/// one of them is a file that would otherwise not play or, worse, run.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// The application the user has set as the default for `mime`, if it is one
/// this machine actually has.
fn preferred<'a>(mime: &str, categories: &'a [Category]) -> Option<&'a App> {
    for list in mimeapps_files() {
        let Ok(raw) = std::fs::read_to_string(&list) else {
            continue;
        };
        for id in defaults(&raw, mime) {
            if let Some(app) = entry_named(&id, categories) {
                return Some(app);
            }
        }
    }
    None
}

/// Every `mimeapps.list` that has a say, in the order the spec gives them.
///
/// The desktop-specific files (`linexinbar-mimeapps.list`) are not looked for.
/// This desktop ships one now, and there is a single line in it: which entry
/// opens a folder. That line is for everything on the machine *outside* this
/// shell — `xdg-open`, and the applications that fall back to launching
/// whatever opens one — and it names a desktop entry that does nothing but ask
/// the running shell to show the folder. Inside the shell a folder is a column
/// to step into and is never opened *with* anything, so reading that file here
/// could only answer a question this shell does not ask. A file named after
/// some other desktop is that desktop's answer rather than this one's. See
/// `share/applications/linexinbar-mimeapps.list`, and [`crate::reveal`].
fn mimeapps_files() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        dirs.push(config);
    } else if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".config"));
    }
    let config_dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".to_string());
    dirs.extend(
        config_dirs
            .split(':')
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from),
    );

    let mut files: Vec<PathBuf> = dirs
        .into_iter()
        .map(|dir| dir.join("mimeapps.list"))
        .collect();
    // The older location, beside the desktop entries themselves. Still written
    // by some installers, and still read by everything else.
    files.extend(
        crate::xdg_data_dirs("applications")
            .into_iter()
            .map(|dir| dir.join("mimeapps.list")),
    );
    files
}

/// The desktop-entry names a `mimeapps.list` gives as the default for `mime`.
///
/// Only `[Default Applications]`: the other groups say what an application
/// *can* open, which the entries themselves already say, and this is only
/// asked for the one thing they cannot — which of them the user picked.
fn defaults(raw: &str, mime: &str) -> Vec<String> {
    let mut in_group = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_group = line == "[Default Applications]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == mime {
                return value
                    .split(';')
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .collect();
            }
        }
    }
    Vec::new()
}

/// The installed application whose desktop entry is called `id`.
pub fn entry_named<'a>(id: &str, categories: &'a [Category]) -> Option<&'a App> {
    apps_in(categories).find(|app| {
        app.path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name == id)
    })
}

/// The installed applications that open `mime`, best first, for a user who has
/// never said which they want.
///
/// Ranked rather than taken in any order the catalogue gives, because
/// "declares the type" is a much weaker claim than it sounds: an audio editor
/// opens `audio/flac` as surely as a music player does, and the alphabet put
/// Audacity ahead of VLC on the machine this was written on. Pressing a song
/// and getting a waveform editor is not a defensible default, and pressing a
/// photograph and getting GIMP is the same mistake in the other column.
///
/// So `Player` and `Viewer` — the menu spec's own additional categories for
/// exactly this, declared by VLC, mpv and Gwenview — come first, an
/// application that says nothing either way comes next, and something that
/// calls itself an editor comes last.
///
/// A stable sort, so within one rank the catalogue's own order survives — which
/// is column order and then alphabetical, and therefore the same list on every
/// start rather than whichever the disk answered with first. The whole list
/// rather than the best of it, because the Open with menu offers all of them
/// and the two must not be able to disagree about which is best.
fn ranked<'a>(mime: &str, categories: &'a [Category]) -> Vec<&'a App> {
    let mut apps: Vec<&App> = apps_in(categories)
        .filter(|app| app.mime_types.iter().any(|own| own == mime))
        .collect();
    apps.sort_by_key(|app| handler_rank(app));
    apps
}

/// How much an application looks like something to *open* a file with, as
/// against something to work on it with.
fn handler_rank(app: &App) -> u8 {
    let says = |name: &str| app.categories.iter().any(|own| own == name);
    if says("Player") || says("Viewer") {
        0
    } else if app.categories.iter().any(|own| own.ends_with("Editing")) {
        2
    } else {
        1
    }
}

/// Every installed application on the machine, in the catalogue's own order.
///
/// What the *Other application* list is built from. The catalogue's order is
/// column order and then alphabetical, so it is the same list on every start
/// rather than whichever order the disk answered in — and it is deliberately
/// not [`ranked`]'s order: that one is answering "which of these is best for
/// this type", and somebody reading this list has already decided which
/// program they want and is looking for its name.
///
/// Nothing is filtered. A file with no extension is the case this exists for,
/// nothing declares it, and a list that left out the applications which say
/// they open nothing would leave out most of what somebody would reach for.
pub fn every_application(categories: &[Category]) -> Vec<&App> {
    apps_in(categories).collect()
}

/// Every installed application in the catalogue, subcategories included.
fn apps_in(categories: &[Category]) -> impl Iterator<Item = &App> {
    let mut apps: Vec<&App> = Vec::new();
    for category in categories {
        crate::apps::walk(&category.entries, &mut |entry| {
            if let Some(app) = entry.app() {
                apps.push(app);
            }
        });
    }
    apps.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::Entry;

    fn file(path: &str) -> Option<File> {
        File::at(Path::new(path))
    }

    /// The same, as a shelf holds it.
    fn shelved(path: &str) -> Shelved {
        Arc::new(file(path).expect("a listable file"))
    }

    #[test]
    fn a_file_is_recognised_by_its_extension_whatever_its_case() {
        let song = file("/home/x/Music/Ok Computer/02 Paranoid Android.FLAC").unwrap();
        assert_eq!(song.kind, Kind::Audio);
        assert_eq!(song.title, "02 Paranoid Android");
        assert_eq!(song.mime, "audio/flac");

        let film = file("/home/x/Videos/holiday.MKV").unwrap();
        assert_eq!(film.kind, Kind::Video);
        assert_eq!(film.mime, "video/x-matroska");

        let picture = file("/home/x/Pictures/Sunset.JPEG").unwrap();
        assert_eq!(picture.kind, Kind::Image);
        assert_eq!(picture.title, "Sunset");
        assert_eq!(picture.mime, "image/jpeg");

        // And nothing else is listed, however much it looks like media.
        assert!(file("/home/x/notes.txt").is_none());
        assert!(file("/home/x/favicon.ico").is_none());
        assert!(file("/home/x/Music").is_none());
    }

    /// The one extension that would otherwise fill Video with source code.
    #[test]
    fn typescript_is_not_a_film() {
        assert!(file("/home/x/project/src/main.ts").is_none());
        assert!(file("/home/x/project/src/main.mts").is_none());
        // The unambiguous transport stream still is one.
        assert!(file("/home/x/Videos/camera.m2ts").is_some());
    }

    #[test]
    fn the_shelf_is_alphabetical_however_the_walk_arrived() {
        let mut shelf = Vec::new();
        shelve(
            &mut shelf,
            vec![shelved("/home/x/b.mp3"), shelved("/home/x/A.mp3")],
        );
        shelve(
            &mut shelf,
            vec![shelved("/home/x/z.mp3"), shelved("/home/x/c.mp3")],
        );
        let titles: Vec<&str> = shelf.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(titles, ["A", "b", "c", "z"]);

        // A second batch merged into a shelf that is already in order goes
        // *into* it rather than onto the end.
        shelve(&mut shelf, vec![shelved("/home/x/bb.mp3")]);
        let titles: Vec<&str> = shelf.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(titles, ["A", "b", "bb", "c", "z"]);
    }

    /// Two files of the same name in different folders are two rows, in a
    /// fixed order.
    #[test]
    fn the_folder_breaks_a_tie_and_says_which_is_which() {
        let mut shelf = Vec::new();
        shelve(
            &mut shelf,
            vec![
                shelved("/home/x/Music/live/Song.mp3"),
                shelved("/home/x/Music/album/Song.mp3"),
            ],
        );
        assert_eq!(shelf.len(), 2);
        assert!(shelf[0].path.ends_with("album/Song.mp3"));
        assert_ne!(shelf[0].folder, shelf[1].folder);
    }

    /// Empty is two different states and the row has to say which.
    #[test]
    fn an_empty_shelf_says_whether_it_is_still_looking() {
        assert_eq!(
            note(Kind::Audio, 0, false),
            "Looking through your home folder"
        );
        assert_eq!(
            note(Kind::Audio, 0, true),
            "No audio files in your home folder"
        );
        assert_eq!(
            note(Kind::Video, 1, true),
            "1 video file in your home folder"
        );
        assert_eq!(note(Kind::Video, 4, false), "4 video files so far");
        // And a large shelf is a large shelf. There is no count at which the
        // bar stops listing, so there is nothing for the row to apologise for.
        assert_eq!(
            note(Kind::Image, 250_000, true),
            "250000 images in your home folder"
        );
    }

    /// A file with the facts written on it, instead of whatever happens to be
    /// on the disk of the machine running the tests. `age` is seconds after the
    /// epoch; `None` is a filesystem that does not keep that time.
    fn facts(path: &str, size: u64, created: Option<u64>, modified: Option<u64>) -> Shelved {
        let seconds =
            |at: Option<u64>| at.map(|at| SystemTime::UNIX_EPOCH + Duration::from_secs(at));
        Arc::new(File {
            size,
            created: seconds(created),
            modified: seconds(modified),
            ..file(path).unwrap()
        })
    }

    fn listed(shelves: &Shelves, kind: Kind) -> Vec<String> {
        shelves
            .listing(kind)
            .into_iter()
            .map(|file| file.title.clone())
            .collect()
    }

    /// Every order the Sort menu offers, against one shelf built so that no two
    /// of them agree — the names, the sizes, the types and both dates each put
    /// the three files in a different order.
    #[test]
    fn a_shelf_is_listed_in_whichever_order_was_asked_for() {
        let mut shelves = Shelves::holding(vec![
            facts("/home/x/beta.jpg", 300, Some(3_000), Some(1_000)),
            facts("/home/x/alpha.png", 100, Some(2_000), Some(3_000)),
            facts("/home/x/gamma.gif", 200, Some(1_000), Some(2_000)),
        ]);

        // The shelf itself is alphabetical whatever is asked of it, because
        // that is the order it is merged in.
        let kept: Vec<&str> = shelves
            .shelf(Kind::Image)
            .iter()
            .map(|file| file.title.as_str())
            .collect();
        assert_eq!(kept, ["alpha", "beta", "gamma"]);

        let order = |shelves: &mut Shelves, sort| {
            shelves.set_sort(Kind::Image, sort);
            listed(shelves, Kind::Image)
        };
        assert_eq!(
            order(&mut shelves, Sort::NameAscending),
            ["alpha", "beta", "gamma"]
        );
        assert_eq!(
            order(&mut shelves, Sort::NameDescending),
            ["gamma", "beta", "alpha"]
        );
        assert_eq!(
            order(&mut shelves, Sort::LargestFirst),
            ["beta", "gamma", "alpha"]
        );
        assert_eq!(
            order(&mut shelves, Sort::SmallestFirst),
            ["alpha", "gamma", "beta"]
        );
        // By mime type: image/gif, image/jpeg, image/png.
        assert_eq!(order(&mut shelves, Sort::Type), ["gamma", "beta", "alpha"]);
        assert_eq!(
            order(&mut shelves, Sort::NewestFirst),
            ["beta", "alpha", "gamma"]
        );
        assert_eq!(
            order(&mut shelves, Sort::OldestFirst),
            ["gamma", "alpha", "beta"]
        );
        assert_eq!(
            order(&mut shelves, Sort::LastChangedFirst),
            ["alpha", "gamma", "beta"]
        );
        assert_eq!(
            order(&mut shelves, Sort::LongestUntouchedFirst),
            ["beta", "gamma", "alpha"]
        );

        // Choosing the order a shelf is already in is not news, and the other
        // two shelves were never asked about.
        assert!(!shelves.set_sort(Kind::Image, Sort::LongestUntouchedFirst));
        assert_eq!(shelves.sort(Kind::Audio), Sort::NameAscending);
    }

    /// Files of equal size, or written in the same second, fall back to the
    /// alphabet — so an order is one order and not whichever the walk happened
    /// to hand over first.
    #[test]
    fn files_that_tie_are_settled_by_their_names() {
        let mut shelves = Shelves::holding(vec![
            facts("/home/x/two.mp3", 40, Some(7), Some(7)),
            facts("/home/x/one.mp3", 40, Some(7), Some(7)),
            facts("/home/x/three.mp3", 40, Some(7), Some(7)),
        ]);
        for sort in SORTS.iter().filter(|sort| **sort != Sort::NameDescending) {
            shelves.set_sort(Kind::Audio, *sort);
            assert_eq!(
                listed(&shelves, Kind::Audio),
                ["one", "three", "two"],
                "{}",
                sort.label()
            );
        }
    }

    /// A search keeps the files whose *names* hold what was typed, whatever
    /// case either was written in.
    #[test]
    fn a_shelf_lists_only_the_files_whose_names_hold_the_query() {
        let mut shelves = Shelves::holding(vec![
            shelved("/home/x/Music/Radiohead - Creep.mp3"),
            shelved("/home/x/Music/Old RADIO Show.mp3"),
            shelved("/home/x/Music/Let It Be.mp3"),
            // In a folder that matches, with a name that does not. The folder
            // is not searched: what the user typed is what they are looking
            // for, and a whole album answering to the name of the shelf it
            // sits in would bury the track they meant.
            shelved("/home/x/Music/Radio Sessions/Karma Police.mp3"),
        ]);

        assert!(shelves.set_query(Kind::Audio, "radio".to_string()));
        assert_eq!(
            listed(&shelves, Kind::Audio),
            ["Old RADIO Show", "Radiohead - Creep"]
        );

        // Folded on both sides, so the shift key never decides what is found.
        shelves.set_query(Kind::Audio, "RaDiO".to_string());
        assert_eq!(
            listed(&shelves, Kind::Audio),
            ["Old RADIO Show", "Radiohead - Creep"]
        );

        // Anywhere in the name, not only at the start of it.
        shelves.set_query(Kind::Audio, "head".to_string());
        assert_eq!(listed(&shelves, Kind::Audio), ["Radiohead - Creep"]);

        // A search nothing answers is an empty shelf rather than a whole one.
        shelves.set_query(Kind::Audio, "nothing here".to_string());
        assert!(listed(&shelves, Kind::Audio).is_empty());

        // And an empty query is every file again, which is not the same
        // question as "match the empty string" happening to be true of all of
        // them: the shelf is never asked at all.
        assert!(shelves.set_query(Kind::Audio, String::new()));
        assert_eq!(
            listed(&shelves, Kind::Audio),
            [
                "Karma Police",
                "Let It Be",
                "Old RADIO Show",
                "Radiohead - Creep"
            ]
        );

        // Typing the query a shelf already holds is not news.
        assert!(!shelves.set_query(Kind::Audio, String::new()));
    }

    /// The search happens before the order does, and it happens to one shelf.
    #[test]
    fn a_search_narrows_one_shelf_and_the_order_is_put_on_what_is_left() {
        let mut shelves = Shelves::holding(vec![
            shelved("/home/x/a-song.mp3"),
            shelved("/home/x/b-song.mp3"),
            shelved("/home/x/c-tune.mp3"),
            shelved("/home/x/a-song.jpg"),
            shelved("/home/x/holiday-song.mkv"),
        ]);
        shelves.set_query(Kind::Audio, "song".to_string());
        shelves.set_sort(Kind::Audio, Sort::NameDescending);
        assert_eq!(listed(&shelves, Kind::Audio), ["b-song", "a-song"]);

        // Music was searched, so music is what was narrowed. A picture and a
        // film with the same word in their names are on shelves nobody asked
        // about, and they are all still there.
        assert_eq!(listed(&shelves, Kind::Image), ["a-song"]);
        assert_eq!(listed(&shelves, Kind::Video), ["holiday-song"]);
    }

    /// What the row over a searched column says, including the sentence that
    /// has to agree with a count of one.
    #[test]
    fn a_searched_shelf_says_how_much_of_itself_it_is_showing() {
        assert_eq!(
            search_note(Kind::Audio.plural(), 12, 3400),
            "12 of 3400 audio files match"
        );
        assert_eq!(
            search_note(Kind::Video.plural(), 1, 20),
            "1 of 20 video files matches"
        );
        assert_eq!(
            search_note(Kind::Image.plural(), 0, 250_000),
            "No images match"
        );
    }

    /// A file the disk knows no date for goes to the end of a list ordered by
    /// dates — at *both* ends of it, which is the thing `Option`'s own ordering
    /// gets wrong.
    #[test]
    fn a_file_with_no_date_is_last_whichever_way_the_list_runs() {
        let mut shelves = Shelves::holding(vec![
            facts("/home/x/dated.mp4", 1, Some(500), Some(500)),
            facts("/home/x/undated.mp4", 1, None, None),
            facts("/home/x/older.mp4", 1, Some(100), Some(100)),
        ]);
        for sort in [
            Sort::NewestFirst,
            Sort::OldestFirst,
            Sort::LastChangedFirst,
            Sort::LongestUntouchedFirst,
        ] {
            shelves.set_sort(Kind::Video, sort);
            assert_eq!(
                listed(&shelves, Kind::Video).last().map(String::as_str),
                Some("undated"),
                "{}",
                sort.label()
            );
        }
    }

    /// An order the whole shelf has nothing to answer with is not offered.
    /// Several filesystems keep no creation time, and a row that is chosen and
    /// does nothing reads as the shell being broken.
    #[test]
    fn an_order_nothing_on_the_shelf_can_be_put_in_is_refused() {
        // What a shelf of these files would tell the bar it can be ordered by.
        let knows = |files: Vec<Shelved>| Shelves::holding(files).made(Kind::Audio).orders;

        let dated = facts("/home/x/a.mp3", 1, Some(9), Some(9));
        let blank = facts("/home/x/b.mp3", 1, None, None);

        let all = knows(vec![dated.clone()]);
        for sort in SORTS {
            assert!(sort.orders(all), "{}", sort.label());
        }

        let none = knows(vec![blank.clone()]);
        assert!(Sort::NameAscending.orders(none));
        assert!(Sort::LargestFirst.orders(none));
        assert!(Sort::Type.orders(none));
        assert!(!Sort::NewestFirst.orders(none));
        assert!(!Sort::OldestFirst.orders(none));
        assert!(!Sort::LastChangedFirst.orders(none));
        assert!(!Sort::LongestUntouchedFirst.orders(none));

        // One file that knows is enough for the whole shelf to be sortable.
        assert!(Sort::NewestFirst.orders(knows(vec![blank, dated])));
    }

    /// The names in the settings file survive a round trip, and no two orders
    /// share a name or a label — a file that said `size` would be ambiguous,
    /// and two rows reading alike would be two rows nobody could choose between.
    #[test]
    fn every_order_has_its_own_name_and_can_be_read_back() {
        let mut keys: Vec<&str> = SORTS.iter().map(|sort| sort.key()).collect();
        let mut labels: Vec<&str> = SORTS.iter().map(|sort| sort.label()).collect();
        keys.sort();
        labels.sort();
        let unique = |mut names: Vec<&str>| {
            let before = names.len();
            names.dedup();
            names.len() == before
        };
        assert!(unique(keys.clone()), "{keys:?}");
        assert!(unique(labels.clone()), "{labels:?}");

        for sort in SORTS {
            assert_eq!(Sort::from_key(sort.key()), Some(*sort));
        }
        assert_eq!(Sort::from_key("name"), Some(Sort::NameAscending));
        assert_eq!(Sort::from_key("by size"), None);
        assert_eq!(Sort::default(), Sort::NameAscending);
    }

    /// A file that has just been deleted leaves the bar with it, rather than
    /// standing there until the walk comes round again five minutes later.
    #[test]
    fn a_deleted_file_is_taken_off_the_shelf_at_once() {
        let mut shelves = Shelves::holding(vec![
            facts("/home/x/a.mp3", 1, None, None),
            facts("/home/x/b.mp3", 1, None, None),
        ]);
        assert_eq!(
            shelves.forget(Path::new("/home/x/a.mp3")),
            Some(Kind::Audio)
        );
        assert_eq!(listed(&shelves, Kind::Audio), ["b"]);
        assert_eq!(
            shelves.forget(Path::new("/home/x/a.mp3")),
            None,
            "a file that has already gone is not news"
        );
    }

    /// The worker, end to end and on its own thread: it finds what is on the
    /// disk, hands over finished rows, answers a shell that changes its mind
    /// about the order, and stops when the shell does.
    ///
    /// The one test that exercises the arrangement rather than the pieces —
    /// that nothing the shell asks for has to wait for the next pass of a walk,
    /// and that no shelf ever crosses the wire.
    #[test]
    fn the_worker_walks_and_answers_without_the_shell_touching_a_shelf() {
        let home = std::env::temp_dir().join(format!("lxb-worker-{}-{:?}", std::process::id(), {
            std::thread::current().id()
        }));
        let deep = home.join("holiday");
        std::fs::create_dir_all(&deep).expect("a scratch home");
        for (at, name) in [
            (&home, "beta.png"),
            (&home, "alpha.png"),
            (&deep, "gamma.jpg"),
        ] {
            std::fs::write(at.join(name), b"not really a picture").expect("a scratch file");
        }
        std::fs::write(home.join("song.mp3"), b"not really a song").expect("a scratch file");

        let (send_made, made) = mpsc::channel();
        let (asks, take_asks) = mpsc::channel();
        let walking = {
            let home = home.clone();
            std::thread::spawn(move || {
                work(
                    &home,
                    [Sort::default(); 3],
                    Pace::default(),
                    &send_made,
                    &take_asks,
                )
            })
        };

        // What the next delivery of one kind says. The three shelves are filled
        // by one walk and land separately, so anything else that arrives on the
        // way is kept rather than dropped — it is somebody else's answer, not
        // noise.
        let mut waiting: Vec<Made> = Vec::new();
        let mut shelf = |kind: Kind| -> (Vec<String>, String) {
            let deadline = Instant::now() + Duration::from_secs(10);
            let made = loop {
                if let Some(at) = waiting.iter().position(|made| made.kind == kind) {
                    break waiting.remove(at);
                }
                let left = deadline.saturating_duration_since(Instant::now());
                waiting.push(made.recv_timeout(left).expect("the worker answers"));
            };
            let titles = made
                .rows
                .iter()
                .map(|row| crate::apps::Entry::title(row).to_string())
                .collect();
            (titles, made.note)
        };

        // Everywhere under the scratch home, alphabetically, whatever folder —
        // under the field that searches them, which the worker builds along
        // with the rows because it is the only one that can count them.
        let (titles, note) = shelf(Kind::Image);
        assert_eq!(titles, ["Search", "alpha", "beta", "gamma"]);
        assert_eq!(note, "3 images in your home folder");

        // A different order is answered at once rather than at the next pass of
        // the walk, which is five minutes away.
        asks.send(Ask::Sort(Kind::Image, Sort::NameDescending))
            .expect("the worker is listening");
        let (titles, _) = shelf(Kind::Image);
        assert_eq!(titles, ["Search", "gamma", "beta", "alpha"]);

        // A search is answered on the same terms, and the row that says what
        // it did is built with it: the query stands in for the word "Search",
        // and the row under it offers the whole shelf back.
        asks.send(Ask::Search(Kind::Image, "ph".to_string()))
            .expect("the worker is listening");
        let (titles, note) = shelf(Kind::Image);
        assert_eq!(titles, ["ph", "Clear search", "alpha"]);
        assert_eq!(note, "1 of 3 images matches");

        // Taking it back is the same ask with nothing in it, and the row that
        // offered it goes with it.
        asks.send(Ask::Search(Kind::Image, String::new()))
            .expect("the worker is listening");
        let (titles, note) = shelf(Kind::Image);
        assert_eq!(titles, ["Search", "gamma", "beta", "alpha"]);
        assert_eq!(note, "3 images in your home folder");

        // And so is a file the user has just deleted.
        asks.send(Ask::Forget(home.join("beta.png")))
            .expect("the worker is listening");
        let (titles, note) = shelf(Kind::Image);
        assert_eq!(titles, ["Search", "gamma", "alpha"]);
        assert_eq!(note, "2 images in your home folder");

        // The music was on its own shelf all along, and the search never
        // crossed to it: a shelf is searched, not the collection.
        let (titles, note) = shelf(Kind::Audio);
        assert_eq!(titles, ["Search", "song"]);
        assert_eq!(note, "1 audio file in your home folder");

        // And the worker stops when the shell lets go, rather than sitting out
        // the five minutes to its next pass.
        drop(asks);
        drop(made);
        let stopped = Instant::now();
        walking.join().expect("the worker ends cleanly");
        assert!(stopped.elapsed() < Duration::from_secs(5));

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The two ways a file can reach a shelf without waiting out the five
    /// minutes to the next pass: the session hands over one it has just
    /// written, and somebody stepping into a column brings the walk forward.
    ///
    /// Both are what a user watching themselves take a screenshot expects, and
    /// neither used to happen. The walk here is paced for a test — its next
    /// pass is a minute away, so a shelf that filled in on its own would prove
    /// nothing — and the file that appears out of nowhere is the one a
    /// recording would be.
    #[test]
    fn a_new_file_reaches_the_shelf_without_waiting_for_the_next_walk() {
        let home = std::env::temp_dir().join(format!("lxb-again-{}-{:?}", std::process::id(), {
            std::thread::current().id()
        }));
        std::fs::create_dir_all(&home).expect("a scratch home");
        std::fs::write(home.join("holiday.png"), b"not really a picture").expect("a scratch file");

        let (send_made, made) = mpsc::channel();
        let (asks, take_asks) = mpsc::channel();
        let pace = Pace {
            again: Duration::from_secs(60),
            soon: Duration::from_millis(50),
        };
        let walking = {
            let home = home.clone();
            std::thread::spawn(move || {
                work(&home, [Sort::default(); 3], pace, &send_made, &take_asks)
            })
        };

        let mut waiting: Vec<Made> = Vec::new();
        let mut images = || -> Vec<String> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let made = loop {
                if let Some(at) = waiting.iter().position(|made| made.kind == Kind::Image) {
                    break waiting.remove(at);
                }
                let left = deadline.saturating_duration_since(Instant::now());
                waiting.push(made.recv_timeout(left).expect("the worker answers"));
            };
            made.rows
                .iter()
                .map(|row| crate::apps::Entry::title(row).to_string())
                .collect()
        };
        assert_eq!(images(), ["Search", "holiday"]);

        // A screenshot: on the disk, and named to the worker in the same
        // breath. It is on the shelf on the next delivery rather than at the
        // next pass of the walk.
        let shot = home.join("Screenshot 2026-08-11 01-15-06.png");
        std::fs::write(&shot, b"not really a picture").expect("a scratch file");
        asks.send(Ask::Found(shot.clone()))
            .expect("the worker is listening");
        assert_eq!(
            images(),
            ["Search", "holiday", "Screenshot 2026-08-11 01-15-06"]
        );

        // A file nothing told the worker about — a recording an application
        // wrote — is found by the walk, and stepping into the column is what
        // brings that walk forward from a minute away to now.
        std::fs::write(home.join("capture.mkv"), b"not really a film").expect("a scratch file");
        asks.send(Ask::LookAgain).expect("the worker is listening");
        let deadline = Instant::now() + Duration::from_secs(10);
        let films = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let made = made.recv_timeout(left).expect("the walk comes round");
            if made.kind == Kind::Video {
                break made
                    .rows
                    .iter()
                    .map(|row| crate::apps::Entry::title(row).to_string())
                    .collect::<Vec<_>>();
            }
        };
        assert_eq!(films, ["Search", "capture"]);

        // And that walk met the screenshot it had already been handed without
        // shelving it twice. Asked for in the other order, because the walk
        // finding a file it already knows about is not news and delivers
        // nothing — where a second copy of it would have been.
        asks.send(Ask::Sort(Kind::Image, Sort::NameDescending))
            .expect("the worker is listening");
        assert_eq!(
            images(),
            ["Search", "Screenshot 2026-08-11 01-15-06", "holiday"],
            "one file is one row, however many ways the worker heard about it"
        );

        drop(asks);
        drop(made);
        walking.join().expect("the worker ends cleanly");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Building the rows of a large shelf is itself expensive, so the worker
    /// leaves room between deliveries in proportion to what the last one cost.
    #[test]
    fn a_shelf_that_costs_something_to_build_is_handed_over_less_often() {
        // A small collection: the floor decides, and the bar fills in front of
        // the user four times a second.
        assert_eq!(wait_after(Duration::ZERO), PUBLISH);
        assert_eq!(wait_after(Duration::from_millis(10)), PUBLISH);
        // A large one: at most a quarter of the worker goes into rebuilding.
        assert_eq!(
            wait_after(Duration::from_millis(100)),
            Duration::from_millis(400)
        );
    }

    #[test]
    fn a_path_that_cannot_be_written_down_is_not_listed() {
        use std::os::unix::ffi::OsStrExt;
        let raw = OsStr::from_bytes(b"/home/x/\xff\xfe.mp3");
        assert!(File::at(Path::new(raw)).is_none());
    }

    /// Everything a music collection is full of: spaces, apostrophes, brackets
    /// and dollar signs. Each of them is a file that has to play, and one of
    /// them is a file that must not run anything.
    #[test]
    fn a_name_a_shell_would_have_eaten_is_quoted_whole() {
        assert_eq!(
            quoted(Path::new("/home/x/Don't Stop.mp3")),
            r"'/home/x/Don'\''t Stop.mp3'"
        );
        assert_eq!(
            quoted(Path::new("/home/x/$(reboot) [live].mp3")),
            "'/home/x/$(reboot) [live].mp3'"
        );
    }

    fn app(name: &str, entry: &str, exec: &str, mime: &[&str]) -> App {
        App {
            name: name.to_string(),
            comment: None,
            icon: None,
            exec: exec.to_string(),
            terminal: false,
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: mime.iter().map(|m| m.to_string()).collect(),
            path: PathBuf::from(format!("/usr/share/applications/{entry}")),
            wm_class: None,
        }
    }

    fn filed(mut app: App, categories: &[&str]) -> App {
        app.categories = categories.iter().map(|c| c.to_string()).collect();
        app
    }

    fn catalogue(apps: Vec<App>) -> Vec<Category> {
        vec![Category {
            id: "multimedia",
            title: "Multimedia",
            icon: "m",
            entries: apps.into_iter().map(Entry::App).collect(),
        }]
    }

    #[test]
    fn the_default_the_user_set_wins_over_one_that_merely_claims_the_type() {
        let categories = catalogue(vec![
            app(
                "Celluloid",
                "io.github.celluloid.desktop",
                "celluloid",
                &["audio/mpeg"],
            ),
            app(
                "VLC",
                "vlc.desktop",
                "vlc --started-from-file",
                &["audio/mpeg"],
            ),
        ]);
        let song = file("/home/x/a.mp3").unwrap();

        // Nothing chosen: the catalogue's own order decides, so the answer is
        // the same on every start.
        assert_eq!(ranked(song.mime, &categories)[0].name, "Celluloid");

        // Chosen: that one, and the command carries the file.
        let list = "[Added Associations]\naudio/mpeg=celluloid.desktop;\n\
                    [Default Applications]\naudio/mpeg=vlc.desktop;celluloid.desktop\n";
        let chosen = defaults(list, "audio/mpeg");
        assert_eq!(chosen, ["vlc.desktop", "celluloid.desktop"]);
        let app = entry_named(&chosen[0], &categories).unwrap();
        assert_eq!(app.name, "VLC");
        assert_eq!(
            format!("{} {}", app.exec, quoted(&song.path)),
            "vlc --started-from-file '/home/x/a.mp3'"
        );
    }

    /// The Open with list and the plain Open row must not be able to disagree:
    /// the first row of the one is what the other would have started.
    ///
    /// Asserted against whichever application it turns out to be, rather than
    /// against a name, because the answer depends on this machine's own
    /// `mimeapps.list` — and the property being tested holds either way.
    #[test]
    fn the_open_with_list_starts_with_what_a_plain_open_would_use() {
        let categories = catalogue(vec![
            filed(
                app("Audacity", "audacity.desktop", "audacity", &["audio/flac"]),
                &["AudioVideoEditing"],
            ),
            filed(
                app("VLC", "vlc.desktop", "vlc", &["audio/flac"]),
                &["Player"],
            ),
            app("Something", "s.desktop", "s", &["audio/flac"]),
        ]);
        let song = file("/home/x/a.flac").unwrap();
        let offered = handlers(song.mime, &categories);

        // Every application that opens the type, each of them once: a default
        // the user has set heads the list rather than appearing twice in it.
        let names: Vec<&str> = offered.iter().map(|app| app.name.as_str()).collect();
        let mut once = names.clone();
        once.sort();
        once.dedup();
        assert_eq!(once.len(), names.len(), "{names:?}");
        assert_eq!(names.len(), 3, "{names:?}");

        let opening = opening(&song.path, song.mime, &categories).unwrap();
        assert_eq!(opening.name, names[0]);
        assert!(opening.command.ends_with("'/home/x/a.flac'"));
        assert_eq!(opening.icon, offered[0].icon);

        // And nothing at all for a type nothing installed claims, which is what
        // greys the Open with row out.
        let film = file("/home/x/a.mkv").unwrap();
        assert!(handlers(film.mime, &categories).is_empty());
    }

    /// A default naming something that is not installed is not an answer.
    #[test]
    fn a_default_for_an_application_that_is_gone_falls_through() {
        let categories = catalogue(vec![app("VLC", "vlc.desktop", "vlc", &["audio/mpeg"])]);
        assert!(entry_named("mpv.desktop", &categories).is_none());
        assert!(ranked("video/mp4", &categories).is_empty());
    }

    /// The machine this was written on, exactly: Audacity claims `audio/flac`,
    /// declares itself an editor, and sorts before VLC. Nothing had been set as
    /// a default, so the alphabet was choosing — and pressing a song opened a
    /// waveform editor.
    #[test]
    fn with_no_default_set_a_player_beats_an_editor() {
        let categories = catalogue(vec![
            filed(
                app("Audacity", "audacity.desktop", "audacity", &["audio/flac"]),
                &["AudioVideo", "Audio", "AudioVideoEditing"],
            ),
            filed(
                app("VLC", "vlc.desktop", "vlc", &["audio/flac"]),
                &["AudioVideo", "Player", "Recorder"],
            ),
        ]);
        assert_eq!(ranked("audio/flac", &categories)[0].name, "VLC");

        // An application that says nothing either way still beats an editor,
        // and still loses to something that calls itself a player.
        let quiet = catalogue(vec![
            filed(
                app("Audacity", "audacity.desktop", "audacity", &["audio/flac"]),
                &["AudioVideoEditing"],
            ),
            app("Something", "s.desktop", "s", &["audio/flac"]),
        ]);
        assert_eq!(ranked("audio/flac", &quiet)[0].name, "Something");
    }

    /// A flatpak is handed the file inside its forwarding markers, or it opens
    /// a path that does not exist inside its own sandbox.
    #[test]
    fn a_flatpak_gets_the_file_where_it_forwards_them() {
        // What `strip_field_codes` leaves of a real flatpak entry, whose
        // `Exec` is `flatpak run --file-forwarding org.x.App @@u %u @@`.
        assert_eq!(
            with_file(
                "flatpak run --file-forwarding org.x.App @@u @@",
                Path::new("/home/x/a.mp3")
            ),
            "flatpak run --file-forwarding org.x.App @@u '/home/x/a.mp3' @@"
        );
        // And everything else takes it on the end, where its own arguments end.
        assert_eq!(
            with_file("vlc --started-from-file", Path::new("/home/x/a.mp3")),
            "vlc --started-from-file '/home/x/a.mp3'"
        );
    }

    /// Choosing an application off the Open with list writes it down, and the
    /// rest of the file is the user's — every group the shell does not
    /// understand survives, and so does everything in the one it does.
    #[test]
    fn a_chosen_application_is_written_into_the_users_own_list() {
        // Nothing chosen for anything, ever: the group is made.
        let fresh = with_default("", "audio/flac", "mpv.desktop");
        assert_eq!(fresh, "[Default Applications]\naudio/flac=mpv.desktop\n");
        assert_eq!(defaults(&fresh, "audio/flac"), ["mpv.desktop"]);

        // A file with other groups in it: the group is added at the end and
        // nothing else is touched.
        let list = "[Added Associations]\naudio/flac=audacity.desktop;\n";
        let written = with_default(list, "audio/flac", "mpv.desktop");
        assert!(written.starts_with(list), "{written:?}");
        assert_eq!(defaults(&written, "audio/flac"), ["mpv.desktop"]);

        // A type that already has a default: that one line is replaced, and
        // the types either side of it are left alone.
        let list = "[Default Applications]\n\
                    image/png=gimp.desktop\n\
                    audio/flac=audacity.desktop;vlc.desktop\n\
                    video/mp4=vlc.desktop\n\
                    \n\
                    [Removed Associations]\n\
                    audio/flac=celluloid.desktop\n";
        let written = with_default(list, "audio/flac", "mpv.desktop");
        assert_eq!(defaults(&written, "audio/flac"), ["mpv.desktop"]);
        assert_eq!(defaults(&written, "image/png"), ["gimp.desktop"]);
        assert_eq!(defaults(&written, "video/mp4"), ["vlc.desktop"]);
        assert!(
            written.contains("[Removed Associations]\naudio/flac=celluloid.desktop"),
            "{written:?}"
        );
        assert_eq!(written.lines().count(), list.lines().count());

        // A type with no line of its own goes in with the group rather than
        // after the blank line that ends it.
        let written = with_default(list, "image/jpeg", "gwenview.desktop");
        assert_eq!(defaults(&written, "image/jpeg"), ["gwenview.desktop"]);
        assert_eq!(
            defaults(&written, "audio/flac"),
            ["audacity.desktop", "vlc.desktop"]
        );
        let lines: Vec<&str> = written.lines().collect();
        let group = lines
            .iter()
            .position(|line| *line == "[Removed Associations]");
        assert_eq!(lines[group.unwrap() - 2], "image/jpeg=gwenview.desktop");
    }

    #[test]
    fn defaults_are_read_out_of_their_own_group_only() {
        let list = "[Default Applications]\nvideo/mp4=mpv.desktop\n\
                    [Removed Associations]\nvideo/mp4=vlc.desktop\n";
        assert_eq!(defaults(list, "video/mp4"), ["mpv.desktop"]);
        assert!(defaults(list, "audio/mpeg").is_empty());
        assert!(defaults("[Removed Associations]\nvideo/mp4=x.desktop\n", "video/mp4").is_empty());
    }
}
