//! Getting rid of one of the user's own files, the way the rest of their
//! desktop does it.
//!
//! The bar lists the music, films and photographs under `$HOME`, so the menu
//! over one of those rows has to be able to offer to get rid of it. What it
//! must not do is `unlink`. The shell has no undo, the row it is acting on is a
//! photograph somebody took, and the difference between a wrong press being an
//! annoyance and a wrong press being a loss is entirely this module.
//!
//! So Delete means the freedesktop trash — the same `~/.local/share/Trash` that
//! Dolphin, Nautilus, Thunar and `gio trash` all use. A file put there by the
//! shell can be found and restored from any of them, and from a terminal, by
//! somebody who has never heard of LineXinBar. That interoperability is the
//! whole reason to implement a specification rather than to move the file to a
//! folder of this shell's own choosing.
//!
//! ## The specification, and the two trash directories
//!
//! A trash directory holds `files/` and `info/`. A file is trashed by claiming
//! a name in `info/` — with `O_EXCL`, which is what makes two programs trashing
//! `holiday.mp4` at the same moment produce two entries rather than one lost
//! file — writing a `.trashinfo` under that name saying where the file came
//! from and when it went, and then renaming the file into `files/` under the
//! same name.
//!
//! Which trash directory is the awkward half, and it is awkward for one
//! reason: a rename cannot cross a filesystem. The home trash only works for
//! files that are on the same volume as the home directory, and a home
//! directory with a media drive mounted inside it is an ordinary arrangement —
//! it is, in fact, the arrangement of anybody whose collection is big enough
//! for this bar to be worth having. For those the spec provides a trash at the
//! top of the volume the file is actually on, and this implements both.
//!
//! Copying the file instead is not an option and is not offered. A copy of a
//! forty-gigabyte film into the home directory to "delete" it would fill the
//! disk it was meant to free, and it would not be atomic — a press of the power
//! switch half way through leaves the user with two half-files instead of one
//! whole one.

use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// How many names are tried before giving up on a folder.
///
/// The first collision is ordinary — the user has deleted two files called
/// `IMG_0001.jpg` from two different folders — and the hundredth means
/// something is wrong that trying again will not fix.
const NAMES: u32 = 100;

/// Put `path` in the trash. Returns the trash directory it went to, which is
/// what the shell says out loud afterwards.
///
/// Nothing here removes anything. Every failure leaves the file exactly where
/// it was, which is the one property that matters: a Delete that reports
/// failure and a Delete that reports success must be the only two outcomes
/// there are.
pub fn discard(path: &Path) -> io::Result<PathBuf> {
    discard_into(path, home_trash())
}

/// The same, told where the home trash is.
///
/// Split off so the whole journey can be exercised against a directory made
/// for the purpose, rather than against the trash of whoever is running the
/// tests. Reaching into the environment inside a test would also be reaching
/// into it for every other test running beside it.
fn discard_into(path: &Path, home: Option<PathBuf>) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            crate::i18n::text("label-a-path-with-no-file-name"),
        )
    })?;
    let facts = std::fs::symlink_metadata(path)?;
    let trash = trash_for(path, facts.dev(), home)?;

    let files = trash.dir.join("files");
    let info = trash.dir.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;

    // What the entry will say it came from: the whole path for the home trash,
    // and the path within the volume for a trash at the top of one — so that a
    // drive trashed from and later mounted somewhere else still restores to the
    // right place inside itself.
    let original = match &trash.within {
        Some(top) => path.strip_prefix(top).unwrap_or(path),
        None => path,
    };
    let deleted_at = stamp();

    for attempt in 1..=NAMES {
        let claimed = numbered(name, attempt);
        let ticket = info.join(format!("{claimed}.trashinfo"));
        // `create_new` is `O_EXCL`: the name is claimed by the file being made,
        // not by having looked and found nothing there a moment ago.
        let mut writing = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ticket)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        writing.write_all(ticket_body(original, &deleted_at).as_bytes())?;
        writing.flush()?;
        drop(writing);

        match std::fs::rename(path, files.join(&claimed)) {
            Ok(()) => {
                tracing::info!(
                    file = %path.display(),
                    trash = %trash.dir.display(),
                    as_name = claimed,
                    "moved a file to the trash"
                );
                return Ok(trash.dir);
            }
            Err(err) => {
                // The claim is given back. An `info` entry with nothing in
                // `files` is a trash directory every other desktop's trash
                // viewer would show an empty row for.
                let _ = std::fs::remove_file(&ticket);
                return Err(err);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        crate::i18n::text("label-the-trash-already-holds-a-hundred-files-of-this-na"),
    ))
}

/// A trash directory, and the volume it belongs to if it is not the home one.
struct Trash {
    dir: PathBuf,
    /// The top of the volume, for a trash that is not the home trash — which
    /// is what the paths inside it are written relative to.
    within: Option<PathBuf>,
}

/// Which trash a file on device `dev` goes to.
///
/// The home trash whenever the file is on the same volume as it, because that
/// is the one every desktop looks in first and the one a user knows how to
/// empty. The volume's own otherwise, which is not a fallback so much as the
/// only thing a rename can do.
fn trash_for(path: &Path, dev: u64, home: Option<PathBuf>) -> io::Result<Trash> {
    if let Some(home) = home {
        // The parent, not the trash itself: it may not exist yet, and what is
        // being asked is which volume it *would* be made on.
        let anchor = home.parent().unwrap_or(&home);
        if let Ok(facts) = std::fs::metadata(anchor) {
            if facts.dev() == dev {
                return Ok(Trash {
                    dir: home,
                    within: None,
                });
            }
        }
    }

    let top = top_directory(path, dev).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            crate::i18n::text("label-no-home-trash-and-no-volume-to-make-one-on"),
        )
    })?;
    Ok(Trash {
        dir: volume_trash(&top)?,
        within: Some(top),
    })
}

/// `$XDG_DATA_HOME/Trash`, or the default the spec gives for it.
fn home_trash() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".local/share"))
        })?;
    Some(data.join("Trash"))
}

/// The trash at the top of a volume: the administrator's `.Trash` where there
/// is a usable one, and this user's own `.Trash-1000` otherwise.
///
/// The three conditions on `$topdir/.Trash` are the spec's and every one of
/// them is a way the shared directory could be a trap. It must not be a
/// symbolic link, which could point anywhere; it must be a directory; and it
/// must have the sticky bit, without which any user on the machine could delete
/// another's trashed files or replace them. A `.Trash` that fails any of the
/// three is left alone entirely rather than repaired — it is not this shell's
/// directory to fix.
fn volume_trash(top: &Path) -> io::Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let shared = top.join(".Trash");
    if let Ok(facts) = std::fs::symlink_metadata(&shared) {
        let sticky = facts.permissions().mode() & 0o1000 != 0;
        if facts.is_dir() && !facts.file_type().is_symlink() && sticky {
            let mine = shared.join(uid.to_string());
            std::fs::create_dir_all(&mine)?;
            return Ok(mine);
        }
    }

    let own = top.join(format!(".Trash-{uid}"));
    std::fs::create_dir_all(&own)?;
    // Nobody else's business. The shared directory above is readable by every
    // user on the machine by design; this one is a folder of the user's own
    // deleted files sitting at the top of a drive, and it is made the way the
    // rest of the shell makes private things.
    let _ = std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o700));
    Ok(own)
}

/// Where the volume `path` is on is mounted: the last directory going up that
/// is still on device `dev`.
///
/// Walked rather than read out of `/proc/self/mountinfo`. The device number is
/// what actually decides whether a rename will work, a mount table has to be
/// parsed and matched back to a path, and the walk is a handful of `stat` calls
/// on a path that is already in the page cache.
fn top_directory(path: &Path, dev: u64) -> Option<PathBuf> {
    let mut top = path.parent()?.to_path_buf();
    loop {
        let Some(parent) = top.parent() else {
            return Some(top);
        };
        match std::fs::metadata(parent) {
            Ok(facts) if facts.dev() == dev => top = parent.to_path_buf(),
            // The parent is on another volume, so this directory is where the
            // one the file is on is mounted. An unreadable parent is treated
            // the same way: it is as far as this can see.
            _ => return Some(top),
        }
    }
}

/// The name a file is filed under, once collisions have been counted.
///
/// The number goes on the end of the whole name rather than before the
/// extension, which is what `gio trash` does and therefore what a trash folder
/// already looks like on most machines. It is only ever a name inside `files/`
/// — what the file is called if it is restored is written down separately, and
/// is untouched by this.
fn numbered(name: &std::ffi::OsStr, attempt: u32) -> String {
    let name = name.to_string_lossy();
    match attempt {
        1 => name.into_owned(),
        n => format!("{name}.{n}"),
    }
}

/// What goes in the `.trashinfo` file.
///
/// Two keys and a group header, and the whole of the format. `Path` is escaped
/// the way a URL's path is — by the same rule and with the same reserved set as
/// the thumbnail spec's file URIs, which is why the two share one encoder —
/// because a file called `Don't Stop.mp3` has to survive a round trip through
/// an `ini` file that has no quoting.
fn ticket_body(original: &Path, deleted_at: &str) -> String {
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        crate::thumbs::percent_encoded(original),
        deleted_at
    )
}

/// Now, as the spec writes a deletion date: local time, to the second, in the
/// `YYYY-MM-DDThh:mm:ss` form RFC 3339 gives for a time with no zone on it.
///
/// Local rather than UTC because that is what the format says and what every
/// trash viewer prints back unchanged — a file deleted at nine in the evening
/// should not be listed as having been deleted at seven.
fn stamp() -> String {
    let Some(tm) = crate::local_time() else {
        // A clock that cannot be read is not a reason to refuse to delete
        // anything. The epoch is a date, it is obviously not the real one, and
        // the file still lands where it can be found and restored.
        return "1970-01-01T00:00:00".to_string();
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Whether a path is one the shell will offer to delete at all.
///
/// The trash is for the user's own things. A file that is not under their home
/// directory is somebody else's — a shared music folder on `/srv`, a mounted
/// disc, a system sample — and the row is drawn greyed rather than removed, so
/// the menu keeps its shape wherever it is raised.
///
/// This is about the *offer*, not about what would work: trashing a file on
/// another volume is perfectly possible and is implemented above. It is about
/// not putting a one-press way to delete other people's files into a shell
/// whose whole point is being driven from a sofa with a controller.
pub fn is_the_users_own(path: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    home.is_absolute() && path.starts_with(&home) && path != home
}

// --- what is in there, and the two ways back out ---------------------------

/// One thing somebody deleted, as a row of the Trash column.
///
/// Everything here was read out of the pair of files the spec keeps: the entry
/// in `files/`, and the `.trashinfo` beside it in `info/` that says where it
/// came from. Both paths are held rather than rebuilt from the name, because
/// putting a thing back and destroying it both have to touch exactly the two
/// files this row was made out of — a second attempt to work out which they
/// were is a second chance to work it out wrong, over somebody's photographs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trashed {
    /// What it was called before it was deleted, which is what the row says.
    ///
    /// The name out of the recorded path rather than the name in `files/`: two
    /// files of one name are filed as `holiday.mp4` and `holiday.mp4.2`, and a
    /// column that called the second one that would be showing the user a name
    /// the trash invented rather than the one they gave it.
    pub name: String,
    /// Where it is now: the entry under `files/`.
    pub at: PathBuf,
    /// The ticket beside it under `info/`, which goes when it does.
    pub ticket: PathBuf,
    /// Where it came from, as an absolute path on this machine.
    pub from: PathBuf,
    /// The line under the name — see [`Trashed::describe`], which is where it
    /// is written. Held rather than worked out when the row is drawn, for the
    /// reason [`crate::files::Item`] holds its own: a row is drawn sixty times
    /// a second and read off the disk once.
    pub note: String,
    /// What the ticket says about when it went, as the spec spells a date.
    /// Kept verbatim for the ordering and rewritten for the row; see
    /// [`Trashed::note`].
    pub deleted_at: String,
    pub folder: bool,
    /// How big the entry is, for a file. A folder says nothing: what `stat`
    /// gives for one is the size of its index, not of what is in it.
    pub size: u64,
    /// When the entry itself was last written, for the orders that ask.
    pub modified: Option<std::time::SystemTime>,
}

impl Trashed {
    /// The line under the name: where it came from, and when it went.
    ///
    /// Those two and not the size, which is the one thing about a trashed file
    /// nobody is asking — a row in this column is being looked at to decide
    /// whether to put it back, and what decides that is which of the four
    /// `notes.txt` on this machine it is.
    pub(crate) fn describe(&self) -> String {
        let from = self
            .from
            .parent()
            .map(crate::screenshot::abbreviated)
            .unwrap_or_default();
        match (from.is_empty(), self.when()) {
            (true, None) => String::new(),
            (true, Some(when)) => {
                crate::message!("trash-deleted-when", "when" => when)
            }
            (false, None) => {
                crate::message!("trash-from", "from" => from)
            }
            (false, Some(when)) => {
                crate::message!("trash-from-deleted-when", "from" => from, "when" => when)
            }
        }
    }

    /// The deletion date as this shell writes a date — `12 March 2025` — or
    /// nothing at all for a ticket whose date is not one.
    ///
    /// Written out here rather than handed to `strftime`, for the reason the
    /// rest of the shell's dates are: the session's locale is not the language
    /// the shell is in, and one Polish month in a column of English rows reads
    /// as a bug. The date is already local time — that is what the spec stores
    /// — so there is nothing to convert, only to spell.
    pub fn when(&self) -> Option<String> {
        let (date, _) = self.deleted_at.split_once('T')?;
        let mut parts = date.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month: usize = parts.next()?.parse().ok()?;
        let day: u32 = parts.next()?.parse().ok()?;
        crate::i18n::date(day, month.checked_sub(1)?, year)
    }

    /// The mark the row is drawn with: the folder, or whatever the file's own
    /// name says it is — the same table the explorer draws a listing with, so
    /// a song looks like a song in the trash as well.
    pub fn glyph(&self) -> &'static str {
        if self.folder {
            return crate::icons::FILE_FOLDER;
        }
        crate::files::described(&self.from).1
    }
}

/// A trash directory the shell will read: where it is, and the volume its
/// paths are written relative to.
///
/// The same pair [`Trash`] is, and deliberately a second type: that one is
/// somewhere a file is about to be *put*, and is made on the spot if it is not
/// there. This one is somewhere to look, and a trash directory that does not
/// exist is not one to make — a shell that created `.Trash-1000` at the top of
/// every mounted disk just by listing what had been deleted would be leaving
/// empty folders on other people's drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bin {
    pub dir: PathBuf,
    /// The volume, for a trash at the top of one. The paths inside such a
    /// trash are written relative to it — see [`discard_into`] — so this is
    /// what puts an absolute path back together.
    within: Option<PathBuf>,
}

/// Every trash directory on this machine that exists and has anything in it to
/// find: the home trash, and one at the top of every mounted volume.
///
/// Both, because the home trash is not where everything goes. A file on
/// another volume cannot be renamed into it — see [`trash_for`] — so a shell
/// that listed only `~/.local/share/Trash` would be one where deleting a film
/// off a plugged-in drive made it vanish from a Trash that never mentions it
/// again. Every other desktop's trash gathers the same set, and this is the
/// one place in the shell where they have to agree: a file trashed in Dolphin
/// off a stick has to be in this column, and one trashed here has to be in
/// Dolphin's.
///
/// Nothing is created. A volume with no trash on it contributes nothing, which
/// is also why this is cheap: two `stat`s per mounted filesystem, on the press
/// that opens the column.
pub fn bins() -> Vec<Bin> {
    let mut bins = Vec::new();
    if let Some(home) = home_trash() {
        if home.is_dir() {
            bins.push(Bin {
                dir: home,
                within: None,
            });
        }
    }

    let uid = unsafe { libc::getuid() };
    for top in crate::files::mounted_volumes() {
        // The administrator's shared directory, under the same three
        // conditions putting something in it has to meet. A `.Trash` that
        // fails any of them is not this user's to read out of either.
        let shared = top.join(".Trash").join(uid.to_string());
        // And this user's own beside it, which is where nearly everything on a
        // removable drive actually lands.
        let own = top.join(format!(".Trash-{uid}"));
        for dir in [shared, own] {
            if !dir.is_dir() {
                continue;
            }
            // A volume mounted twice is one volume, and a bind mount inside
            // the home directory would otherwise put the home trash on the
            // list a second time under another name.
            if bins.iter().any(|bin| bin.dir == dir) {
                continue;
            }
            bins.push(Bin {
                dir,
                within: Some(top.clone()),
            });
        }
    }
    bins
}

/// Everything in the trash, newest first.
///
/// Read from the tickets rather than from `files/`: a ticket is what says
/// where an entry came from, and an entry in `files/` with no ticket beside it
/// is one no desktop can restore — it is listed all the same, under the name
/// it is filed as and with nowhere to go back to, because a shell that hid it
/// would be a shell in which somebody's file had disappeared for good and
/// silently.
pub fn listing() -> Vec<Trashed> {
    let mut found = Vec::new();
    for bin in bins() {
        read_bin(&bin, &mut found);
    }
    // Newest first, which is the order somebody looking for what they just
    // deleted wants and the order every trash viewer opens in. The name
    // settles anything the date does not, so the column is the same list twice
    // running rather than whatever the directory answered with.
    found.sort_by(|a: &Trashed, b: &Trashed| {
        b.deleted_at
            .cmp(&a.deleted_at)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.at.cmp(&b.at))
    });
    found
}

/// One trash directory's worth of that.
fn read_bin(bin: &Bin, into: &mut Vec<Trashed>) {
    let files = bin.dir.join("files");
    let Ok(reading) = std::fs::read_dir(bin.dir.join("info")) else {
        return;
    };
    for entry in reading.flatten() {
        let ticket = entry.path();
        if ticket.extension() != Some(std::ffi::OsStr::new("trashinfo")) {
            continue;
        }
        let Some(filed_as) = ticket.file_stem().map(PathBuf::from) else {
            continue;
        };
        let at = files.join(&filed_as);
        let Ok(facts) = std::fs::symlink_metadata(&at) else {
            // A ticket with nothing behind it. It is not shown, and it is not
            // repaired either: another program may be half way through putting
            // a file there this instant, which is exactly what claiming the
            // name with `O_EXCL` before the rename is *for*.
            continue;
        };
        let recorded = std::fs::read_to_string(&ticket)
            .ok()
            .and_then(|body| read_ticket(&body));
        // Where it came from, made absolute: a volume trash writes the path
        // within the volume, so the volume goes back on the front of it.
        let from = match (recorded.as_ref(), bin.within.as_ref()) {
            (Some((path, _)), Some(top)) => top.join(path.strip_prefix("/").unwrap_or(path)),
            (Some((path, _)), None) => path.clone(),
            (None, _) => at.clone(),
        };
        let name = from
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_else(|| filed_as.to_str().unwrap_or_default())
            .to_string();
        let mut row = Trashed {
            name,
            at,
            ticket,
            from,
            note: String::new(),
            deleted_at: recorded.map(|(_, when)| when).unwrap_or_default(),
            folder: facts.is_dir(),
            size: facts.len(),
            modified: facts.modified().ok(),
        };
        row.note = row.describe();
        into.push(row);
    }
}

/// The two things a `.trashinfo` says: where the file came from, and when it
/// went.
///
/// `None` for anything that is not one. The header is required rather than
/// assumed — a file in `info/` with a `.trashinfo` name and no `[Trash Info]`
/// in it is somebody else's file, and reading a path out of it would be this
/// shell offering to restore something to wherever that stray file happened to
/// say.
fn read_ticket(body: &str) -> Option<(PathBuf, String)> {
    let mut in_group = false;
    let mut path = None;
    let mut when = String::new();
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Trash Info]";
            continue;
        }
        if !in_group {
            continue;
        }
        match line.split_once('=') {
            Some(("Path", value)) => path = Some(percent_decoded(value)),
            Some(("DeletionDate", value)) => when = value.to_string(),
            _ => {}
        }
    }
    Some((path?, when))
}

/// The other end of [`crate::thumbs::percent_encoded`].
///
/// Bytes rather than characters, because a file name is bytes: `%C3%A9` is one
/// letter written as two of them, and decoding into a `String` a piece at a
/// time would put two replacement characters where the user's `é` was. What
/// comes out is an `OsString`, which is what a path is made of.
fn percent_decoded(value: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;

    let raw = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' && at + 2 < raw.len() {
            let digits = std::str::from_utf8(&raw[at + 1..at + 3]).ok();
            if let Some(byte) = digits.and_then(|digits| u8::from_str_radix(digits, 16).ok()) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(raw[at]);
        at += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Put one trashed thing back where it came from.
///
/// Returns where it landed, which is not always where it came from: the folder
/// it was deleted out of has had however long to acquire another file of that
/// name, and `rename(2)` would put this one over the top of it without a word.
/// So a taken name is answered the way a copy answers one — the thing goes back
/// beside it as `notes (2).txt` — and the caller says out loud where it went.
/// Refusing instead would leave somebody with a file they cannot get out of the
/// trash except by renaming whatever is in its way first, which is a chore the
/// shell can do for them.
///
/// The folder it came out of is remade if it has gone. A photograph deleted out
/// of `~/Pictures/Holiday` after the rest of that folder was deleted has
/// nowhere to be restored to, and the alternative to making the folder is
/// refusing — which is the shell holding somebody's file hostage to the order
/// they pressed things in.
///
/// The ticket goes last and only if the entry moved, so a restore that fails
/// half way leaves the trash exactly as it was rather than leaving an entry
/// nothing can find.
pub fn restore(item: &Trashed) -> io::Result<PathBuf> {
    let Some(folder) = item.from.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            crate::i18n::text("label-a-trashed-file-with-nowhere-to-go-back-to"),
        ));
    };
    std::fs::create_dir_all(folder)?;
    let to = if std::fs::symlink_metadata(&item.from).is_ok() {
        crate::transfer::free_name(folder, &item.name)?
    } else {
        item.from.clone()
    };
    std::fs::rename(&item.at, &to)?;
    // The entry is out, so the ticket is now one of the orphans `read_bin`
    // skips. A failure here is worth a line in the log and nothing more: the
    // user's file is back, which is the whole of what they asked for.
    if let Err(err) = std::fs::remove_file(&item.ticket) {
        tracing::warn!(
            %err,
            ticket = %item.ticket.display(),
            "restored the file but could not clear its trash entry"
        );
    }
    tracing::info!(from = %item.at.display(), to = %to.display(), "restored from the trash");
    Ok(to)
}

/// Destroy one trashed thing for good.
///
/// The entry first and the ticket second, in that order for the reason
/// [`restore`] gives: a ticket with nothing behind it is skipped by every
/// reader, and an entry with no ticket is a file nothing can put back.
pub fn purge(item: &Trashed) -> io::Result<()> {
    if item.folder
        && !std::fs::symlink_metadata(&item.at)?
            .file_type()
            .is_symlink()
    {
        std::fs::remove_dir_all(&item.at)?;
    } else {
        std::fs::remove_file(&item.at)?;
    }
    let _ = std::fs::remove_file(&item.ticket);
    tracing::info!(file = %item.at.display(), "removed from the trash for good");
    Ok(())
}

/// Destroy all of it. Returns how many went, and the first thing that stopped
/// one going if anything did.
///
/// It keeps going past a failure rather than stopping at the first one. Empty
/// is what the user asked for, one entry on a read-only drive is not a reason
/// to leave the other four hundred where they are, and what they are told
/// afterwards is how far it got — see `Shell::empty_the_trash`.
pub fn empty(listing: &[Trashed]) -> (usize, Option<io::Error>) {
    let mut gone = 0;
    let mut first = None;
    for item in listing {
        match purge(item) {
            Ok(()) => gone += 1,
            Err(err) => {
                tracing::warn!(%err, file = %item.at.display(), "could not empty this out of the trash");
                first.get_or_insert(err);
            }
        }
    }
    (gone, first)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole of the file format, against a name with everything in it a
    /// music collection has: a space, an apostrophe, brackets and a non-ASCII
    /// letter.
    #[test]
    fn the_ticket_says_where_the_file_came_from_and_when_it_went() {
        let body = ticket_body(
            Path::new("/home/x/Music/Don't Stop (Café mix).mp3"),
            "2026-08-09T14:03:05",
        );
        assert_eq!(
            body,
            "[Trash Info]\n\
             Path=/home/x/Music/Don't%20Stop%20(Caf%C3%A9%20mix).mp3\n\
             DeletionDate=2026-08-09T14:03:05\n"
        );
        // The separators stay separators, or nothing could read the path back.
        assert!(body.contains("Path=/home/x/Music/"));
    }

    #[test]
    fn a_taken_name_is_numbered_rather_than_overwritten() {
        let name = std::ffi::OsStr::new("holiday.mp4");
        assert_eq!(numbered(name, 1), "holiday.mp4");
        assert_eq!(numbered(name, 2), "holiday.mp4.2");
        assert_eq!(numbered(name, 17), "holiday.mp4.17");
    }

    /// The date is a date, in the form the spec asks for. What it *says* is the
    /// machine's clock and cannot be asserted; that it is nineteen characters
    /// with the separators in the right places can be.
    #[test]
    fn the_deletion_date_is_written_the_way_the_spec_spells_one() {
        let stamp = stamp();
        assert_eq!(stamp.len(), 19, "{stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[7..8], "-");
        assert_eq!(&stamp[10..11], "T");
        assert_eq!(&stamp[13..14], ":");
        assert_eq!(&stamp[16..17], ":");
        assert!(stamp[..4].chars().all(|digit| digit.is_ascii_digit()));
    }

    /// A whole trip through the two directories, on a real filesystem: the file
    /// leaves, the entry arrives under both names, and a second file of the
    /// same name does not overwrite the first.
    #[test]
    fn a_file_goes_to_the_trash_and_leaves_a_ticket_behind() {
        let Some(scratch) = scratch("trash-round-trip") else {
            return;
        };
        let trash = scratch.join("Trash");
        let files = trash.join("files");
        let info = trash.join("info");

        for round in 1..=2 {
            let song = scratch.join("Don't Stop.mp3");
            std::fs::write(&song, format!("round {round}")).unwrap();
            assert_eq!(
                discard_into(&song, Some(trash.clone())).ok(),
                Some(trash.clone()),
                "round {round}"
            );
            assert!(!song.exists(), "the file is still where it was");
        }

        // Two files of one name are two entries, and the first is untouched.
        assert_eq!(read_dir(&files), ["Don't Stop.mp3", "Don't Stop.mp3.2"]);
        assert_eq!(
            read_dir(&info),
            ["Don't Stop.mp3.2.trashinfo", "Don't Stop.mp3.trashinfo"]
        );
        assert_eq!(
            std::fs::read_to_string(files.join("Don't Stop.mp3")).unwrap(),
            "round 1"
        );

        let ticket = std::fs::read_to_string(info.join("Don't Stop.mp3.trashinfo")).unwrap();
        assert!(ticket.starts_with("[Trash Info]\nPath="), "{ticket}");
        assert!(
            ticket.contains(&format!("Path={}/Don't%20Stop.mp3\n", scratch.display())),
            "{ticket}"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A file the home trash cannot take — because it is on another volume —
    /// goes to a trash at the top of the volume it is actually on, and the
    /// path in its ticket is written relative to that volume.
    ///
    /// The two volumes are faked by pointing the home trash at a directory that
    /// does not exist on any device, which is exactly what `trash_for` has to
    /// cope with on a machine whose `$HOME` is somewhere the shell cannot stat.
    #[test]
    fn a_file_the_home_trash_cannot_take_goes_to_the_volume_s_own() {
        let Some(scratch) = scratch("trash-other-volume") else {
            return;
        };
        let song = scratch.join("holiday.mp4");
        std::fs::write(&song, "film").unwrap();

        let nowhere = scratch.join("no-such-place/Trash");
        let Ok(landed) = discard_into(&song, Some(nowhere)) else {
            return; // No writable volume top; nothing to assert.
        };
        assert!(!song.exists());
        assert!(
            landed
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".Trash"),
            "{}",
            landed.display()
        );
        let ticket =
            std::fs::read_to_string(landed.join("info/holiday.mp4.trashinfo")).unwrap_or_default();
        assert!(ticket.contains("Path="), "{ticket}");
        assert!(
            !ticket.contains("Path=/"),
            "a volume trash writes the path within the volume: {ticket}"
        );

        // Put the volume back as it was found, as far as that can be done
        // without touching anything that was already there: each remove fails
        // harmlessly if the directory holds somebody else's trashed files.
        let _ = std::fs::remove_file(landed.join("files/holiday.mp4"));
        let _ = std::fs::remove_file(landed.join("info/holiday.mp4.trashinfo"));
        let _ = std::fs::remove_dir(landed.join("files"));
        let _ = std::fs::remove_dir(landed.join("info"));
        let _ = std::fs::remove_dir(&landed);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Only the user's own files are offered up to the Delete row.
    #[test]
    fn a_file_outside_the_home_directory_is_not_offered() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        if !home.is_absolute() {
            return;
        }
        assert!(is_the_users_own(&home.join("Music/a.mp3")));
        assert!(!is_the_users_own(Path::new("/srv/music/a.mp3")));
        assert!(!is_the_users_own(&home), "the home directory is not a file");
    }

    /// The two keys of a `.trashinfo`, read back out of one — including the
    /// name that put the encoder there in the first place.
    ///
    /// Round-tripped against the writer rather than asserted against a string
    /// typed here, because the one property that matters is that this shell can
    /// read what this shell wrote. What it must also read is what somebody
    /// else's file manager wrote, which is the second half below.
    #[test]
    fn a_ticket_written_here_reads_back_as_the_path_it_was_written_from() {
        let came_from = Path::new("/home/x/Music/Don't Stop (Café mix).mp3");
        let body = ticket_body(came_from, "2026-08-09T14:03:05");
        assert_eq!(
            read_ticket(&body),
            Some((came_from.to_path_buf(), "2026-08-09T14:03:05".to_string()))
        );

        // What Dolphin and `gio trash` write, which is the same two keys with
        // whitespace and a comment the spec allows.
        let theirs = "[Trash Info]\n\
                      Path=/home/x/Pictures/IMG%5F0001.jpg\n\
                      DeletionDate=2025-03-12T09:00:00\n";
        assert_eq!(
            read_ticket(theirs),
            Some((
                PathBuf::from("/home/x/Pictures/IMG_0001.jpg"),
                "2025-03-12T09:00:00".to_string()
            ))
        );

        // And a file in `info/` that is not one of these is not read at all: a
        // path taken out of somebody's stray file would be a path this shell
        // offered to restore something to.
        assert_eq!(read_ticket("Path=/etc/passwd\n"), None);
        assert_eq!(read_ticket("[Something Else]\nPath=/etc/passwd\n"), None);
    }

    /// The decoder takes bytes rather than characters, which is what a file
    /// name is made of.
    #[test]
    fn a_letter_written_as_two_bytes_comes_back_as_one_letter() {
        assert_eq!(
            percent_decoded("/m/Caf%C3%A9.mp3"),
            PathBuf::from("/m/Café.mp3")
        );
        assert_eq!(percent_decoded("/m/a%20b.mp3"), PathBuf::from("/m/a b.mp3"));
        // A stray per cent that is not an escape is a per cent. Somebody has a
        // file called `100%.txt`, and refusing to read its ticket would be
        // refusing to restore it.
        assert_eq!(percent_decoded("/m/100%.txt"), PathBuf::from("/m/100%.txt"));
        assert_eq!(percent_decoded("/m/a%zz"), PathBuf::from("/m/a%zz"));
    }

    /// A date as the spec stores one, spelt the way the rest of the shell
    /// spells a date — and nothing at all for a ticket whose date is not one.
    #[test]
    fn the_deletion_date_is_read_back_in_the_shells_own_words() {
        let dated = |when: &str| Trashed {
            name: "a.txt".to_string(),
            at: PathBuf::new(),
            ticket: PathBuf::new(),
            from: PathBuf::from("/home/x/a.txt"),
            note: String::new(),
            deleted_at: when.to_string(),
            folder: false,
            size: 0,
            modified: None,
        };
        assert_eq!(
            dated("2025-03-12T09:00:00").when().as_deref(),
            Some("12 March 2025")
        );
        assert_eq!(dated("").when(), None);
        assert_eq!(dated("whenever").when(), None);
        assert_eq!(
            dated("2025-03-12T09:00:00").describe(),
            "From /home/x · deleted 12 March 2025"
        );
    }

    /// The whole journey, on a real filesystem: a file and a folder go to the
    /// trash, are listed out of it under the names they had before, and come
    /// back to where they came from.
    #[test]
    fn what_goes_to_the_trash_can_be_listed_and_put_back() {
        let Some(scratch) = scratch("trash-restore") else {
            return;
        };
        let trash = scratch.join("Trash");
        let bin = Bin {
            dir: trash.clone(),
            within: None,
        };

        let song = scratch.join("Don't Stop.mp3");
        std::fs::write(&song, "music").unwrap();
        let album = scratch.join("Live at Leeds");
        std::fs::create_dir(&album).unwrap();
        std::fs::write(album.join("01.flac"), "track").unwrap();

        for path in [&song, &album] {
            discard_into(path, Some(trash.clone())).unwrap();
        }
        assert!(!song.exists() && !album.exists());

        let mut found = Vec::new();
        read_bin(&bin, &mut found);
        found.sort_by(|a, b| a.name.cmp(&b.name));
        let named: Vec<(&str, bool)> = found
            .iter()
            .map(|item| (item.name.as_str(), item.folder))
            .collect();
        assert_eq!(named, [("Don't Stop.mp3", false), ("Live at Leeds", true)]);
        assert_eq!(found[0].from, song);

        // Back where they came from, contents and all, and the ticket goes
        // with them — a trash still listing a file that is out of it would be
        // a row that restores nothing.
        for item in &found {
            assert_eq!(restore(item).unwrap(), item.from);
        }
        assert_eq!(std::fs::read_to_string(&song).unwrap(), "music");
        assert_eq!(
            std::fs::read_to_string(album.join("01.flac")).unwrap(),
            "track"
        );
        let mut after = Vec::new();
        read_bin(&bin, &mut after);
        assert!(after.is_empty(), "the trash is empty again");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Something already using the name goes on using it: what comes back out
    /// of the trash lands beside it rather than over the top of it.
    ///
    /// The one thing a restore must never do is destroy a file, which is the
    /// whole point of there being a trash at all.
    #[test]
    fn a_restore_never_writes_over_what_is_already_there() {
        let Some(scratch) = scratch("trash-collide") else {
            return;
        };
        let trash = scratch.join("Trash");
        let notes = scratch.join("notes.txt");

        std::fs::write(&notes, "the first one").unwrap();
        discard_into(&notes, Some(trash.clone())).unwrap();
        std::fs::write(&notes, "written since").unwrap();

        let mut found = Vec::new();
        read_bin(
            &Bin {
                dir: trash.clone(),
                within: None,
            },
            &mut found,
        );
        let landed = restore(&found[0]).unwrap();
        assert_eq!(landed, scratch.join("notes (2).txt"));
        assert_eq!(
            std::fs::read_to_string(&notes).unwrap(),
            "written since",
            "what was there is untouched"
        );
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "the first one");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Emptying destroys the entries and their tickets, folders and all, and
    /// says how many went.
    #[test]
    fn emptying_takes_the_files_and_the_tickets_with_them() {
        let Some(scratch) = scratch("trash-empty") else {
            return;
        };
        let trash = scratch.join("Trash");
        for name in ["a.txt", "b.txt"] {
            let file = scratch.join(name);
            std::fs::write(&file, "x").unwrap();
            discard_into(&file, Some(trash.clone())).unwrap();
        }
        let folder = scratch.join("box");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("deep.txt"), "x").unwrap();
        discard_into(&folder, Some(trash.clone())).unwrap();

        let mut found = Vec::new();
        read_bin(
            &Bin {
                dir: trash.clone(),
                within: None,
            },
            &mut found,
        );
        assert_eq!(found.len(), 3);

        let (gone, failed) = empty(&found);
        assert_eq!(gone, 3);
        assert!(failed.is_none());
        assert_eq!(read_dir(&trash.join("files")), Vec::<String>::new());
        assert_eq!(read_dir(&trash.join("info")), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// An entry in `files/` with no ticket beside it is not listed, and an
    /// entry whose ticket is not a ticket is listed under the name it is filed
    /// as with nowhere to go back to.
    ///
    /// Both are how a trash directory looks after another program has been
    /// interrupted in the middle of writing one, and neither is a reason to
    /// show the user nothing.
    #[test]
    fn a_ticket_with_nothing_behind_it_is_not_a_row() {
        let Some(scratch) = scratch("trash-orphan") else {
            return;
        };
        let trash = scratch.join("Trash");
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        std::fs::write(trash.join("info/ghost.txt.trashinfo"), "[Trash Info]\n").unwrap();
        std::fs::write(trash.join("files/stray.txt"), "x").unwrap();
        std::fs::write(trash.join("info/stray.txt.trashinfo"), "nonsense").unwrap();

        let mut found = Vec::new();
        read_bin(
            &Bin {
                dir: trash,
                within: None,
            },
            &mut found,
        );
        let named: Vec<&str> = found.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(named, ["stray.txt"], "the ghost has nothing behind it");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A trash at the top of a volume writes its paths within that volume, so
    /// reading one back has to put the volume in front again.
    #[test]
    fn a_volume_trash_restores_to_a_path_inside_that_volume() {
        let Some(scratch) = scratch("trash-volume") else {
            return;
        };
        let trash = scratch.join(".Trash-1000");
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        std::fs::write(trash.join("files/holiday.mp4"), "film").unwrap();
        std::fs::write(
            trash.join("info/holiday.mp4.trashinfo"),
            ticket_body(Path::new("Films/holiday.mp4"), "2025-01-01T00:00:00"),
        )
        .unwrap();

        let mut found = Vec::new();
        read_bin(
            &Bin {
                dir: trash,
                within: Some(scratch.clone()),
            },
            &mut found,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].from, scratch.join("Films/holiday.mp4"));

        // And the folder it came out of is remade if it has gone, or there
        // would be no way back for anything trashed out of a folder that was
        // deleted afterwards.
        assert_eq!(
            restore(&found[0]).unwrap(),
            scratch.join("Films/holiday.mp4")
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A directory of this test's own under the system's temporary folder, or
    /// `None` where there is nowhere to write — in which case the test that
    /// wanted it says nothing rather than failing.
    fn scratch(name: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lxb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    fn read_dir(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}
