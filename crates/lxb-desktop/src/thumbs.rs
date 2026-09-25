//! Pictures of the user's own files: a frame out of a film, a photograph
//! scaled down.
//!
//! A row that stands for a picture and draws a picture *of a picture* is the
//! one place in this shell where a generic mark is plainly worse than the
//! thing itself. A column of twelve identical film strips says only that there
//! are twelve films; a column of twelve frames says which ones.
//!
//! ## Only what is being looked at
//!
//! Making a thumbnail is expensive — a JPEG has to be decoded, a film has to
//! be seeked into and one frame decoded — and a home directory can hold tens
//! of thousands of both. So nothing is made ahead of time. The shell asks for
//! the handful of rows around the cursor, once a frame, and this hands back
//! what it has; anything the cursor has moved away from is dropped from the
//! atlas and forgotten. Scrolling a thousand photographs therefore costs the
//! same as looking at six.
//!
//! ## The cache is everybody's
//!
//! What is made is written to `$XDG_CACHE_HOME/thumbnails/large`, in the
//! layout the freedesktop thumbnail specification lays down: a PNG named for
//! the MD5 of the file's URI, carrying the source's URI and modification time
//! in `tEXt` chunks so a stale one can be told from a good one.
//!
//! That is not a detail. It is the same cache the user's file manager fills,
//! which means the first time this shell opens Images on a machine where
//! somebody has browsed their photographs in Dolphin, every thumbnail is
//! already there and the walk costs one `read` each. It also means the ones
//! this shell makes are not wasted when they close it.
//!
//! MD5 and the PNG chunk layout are implemented here rather than pulled in as
//! dependencies, for the reason the `.desktop` parser gives: both are small,
//! both are frozen, and the whole of what is needed of them is one digest and
//! two text chunks.
//!
//! ## The other picture a file can be
//!
//! A photograph is also the one file that can stand behind the *whole screen*.
//! When the cursor in Files comes to rest on one, the start screen becomes
//! about that picture, exactly as it becomes about a game when the cursor comes
//! to rest on one in a Steam library — see [`Want::Backdrop`] and
//! [`crate::art::Sight`].
//!
//! It is the same worker and the same rule about only doing what is being
//! looked at, and it is deliberately not the same picture: a thumbnail is 256
//! pixels down its longest edge, and stretching one across a display would be
//! showing somebody a photograph of their photograph. So a backdrop is decoded
//! again at the size the screen wants it. Nothing about that is written to the
//! shared cache — a backdrop is not a thumbnail, no other desktop has a use for
//! one, and the file it was made from is on this disk, so having it back costs
//! a read rather than a download.

use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::UNIX_EPOCH;

use crate::media::Kind;

/// The edge a thumbnail is made to fit inside.
///
/// The spec's `large` size, and what every other desktop's `large` directory
/// holds — which is the whole point of writing there. Big enough for the card
/// a row draws it on at any ordinary display scale, and small enough that two
/// dozen of them resident in the atlas is a few megabytes.
pub const SIZE: u32 = 256;

/// How many requests may be waiting to be worked on.
///
/// The queue is a stack rather than a line: the shell asks for what is under
/// the cursor *now*, and a request from three scroll positions ago is about a
/// row nobody is looking at any more. Past this the oldest are dropped, which
/// is the correct thing to lose.
const QUEUE: usize = 48;

/// How many files are worked on at once.
///
/// Two, because the two costs are different: decoding a photograph is the CPU
/// and starting `ffmpeg` on a film is mostly waiting. One worker makes a
/// column of films fill in visibly one at a time; four would put four decoders
/// on a machine that is also drawing sixty frames a second.
const WORKERS: usize = 2;

/// How long a video thumbnailer is given before it is given up on.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(20);

/// Where into a film the frame is taken from.
///
/// Not the first frame: films open on black, on a fade-in, or on a
/// distributor's logo, and a column of black rectangles is worse than a column
/// of film strips. A little way in is where there is something to see.
const INTO_FILM: &str = "3";

/// A thumbnail, ready for the atlas.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA, as the atlas takes it.
    pub rgba: Vec<u8>,
}

/// Which picture of a file is being asked for.
///
/// A file can be asked about twice at once and the two answers are different
/// sizes for different places on the screen, so this travels with every
/// request and with everything that comes back — a backdrop arriving must not
/// be mistaken for the row's thumbnail, and a file that has no thumbnail is not
/// thereby a file that has no backdrop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Want {
    /// The picture on the row: a frame out of a film, a photograph scaled to
    /// [`SIZE`], cached where every other desktop caches one.
    Thumbnail,
    /// The picture behind the whole display, for a photograph the cursor is
    /// standing on in Files. Pictures only — there is no such thing as a film
    /// behind the bar — and nothing is cached.
    Backdrop,
    /// The same, blurred, for a screenshot of one of somebody's own games.
    ///
    /// Its own want rather than a flag on [`Want::Backdrop`], because the two
    /// answers are different pictures of the same file and a shell that could
    /// not tell them apart would put one where the other belongs. What makes it
    /// a different picture is that the source is a screenshot of a console —
    /// three hundred pixels tall, and a wall of squares if it is enlarged
    /// honestly. See [`crate::art::blurred_scenery_from`].
    Snapshot,
    /// A game's logo, at its own shape inside [`crate::art::LOGO_SIZE`] rather
    /// than inside [`SIZE`]: the loading screen draws it a third of a display
    /// wide, and a thumbnail blown up that far is a smear. An Epic game's,
    /// which is a file the helper fetched into this shell's cache — a Steam
    /// game's comes through [`crate::art`] instead. Nothing is cached.
    Logo,
}

/// One finished picture of a file.
pub enum Made {
    Thumbnail(Picture),
    Backdrop(crate::art::Scenery),
    /// The blurred one. Two variants carrying the same thing, so that what
    /// comes back says which of the two pictures of that file it is: the sight
    /// it is filed under downstream differs, and a backdrop put into the
    /// snapshot's layer would be a photograph nobody asked to have blurred.
    Snapshot(crate::art::Scenery),
    Logo(Picture),
}

/// The worker pool, and what it has been asked for.
pub struct Thumbs {
    queue: Arc<Queue>,
    done: Receiver<(Job, Option<Made>)>,
    /// Asked for and not yet answered, so a row on screen for a hundred frames
    /// is asked for once.
    asked: HashSet<Job>,
    /// Answered with nothing: a format nothing installed decodes, a file that
    /// has gone, a video thumbnailer that is not installed. Kept so the shell
    /// does not spend the rest of the session failing at the same file once a
    /// frame — the row keeps its glyph, which is a perfectly good answer.
    ///
    /// By what was wanted as well as by the file, because the two can differ:
    /// a film has a thumbnail and never a backdrop, and a raw from a camera
    /// that nothing here decodes has neither for the same reason.
    barren: HashSet<Job>,
}

/// One thing to make: which file, and which of its pictures.
type Job = (PathBuf, Want);

/// What the workers take their work from.
struct Queue {
    /// Most recently wanted at the front.
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

impl Thumbs {
    /// Start the workers. They block until there is something to do, so this
    /// costs two idle threads on a machine whose owner never opens Images.
    pub fn start() -> Thumbs {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        });
        let (send, done) = mpsc::channel();
        for _ in 0..WORKERS {
            let queue = Arc::clone(&queue);
            let send = send.clone();
            std::thread::spawn(move || work(&queue, &send));
        }
        Thumbs {
            queue,
            done,
            asked: HashSet::new(),
            barren: HashSet::new(),
        }
    }

    /// Ask for one picture of `path`, unless it is already being made or has
    /// already been found to have none.
    pub fn want(&mut self, path: &Path, want: Want) {
        let job = (path.to_path_buf(), want);
        if self.barren.contains(&job) || !self.asked.insert(job.clone()) {
            return;
        }
        let Ok(mut jobs) = self.queue.jobs.lock() else {
            return;
        };
        jobs.push_front(job);
        jobs.truncate(QUEUE);
        drop(jobs);
        self.queue.ready.notify_one();
    }

    /// Whether one picture of a file has already been found not to exist.
    ///
    /// What it is for is the backdrop. A row with no thumbnail keeps its glyph
    /// and nothing else about the screen changes, so nothing has to ask. A
    /// display waiting for a backdrop is holding the *previous* picture on
    /// screen until this one arrives, so a picture that is never coming has to
    /// be said out loud — otherwise the last photograph somebody looked at
    /// stays behind the bar for the rest of the session.
    pub fn hopeless(&self, path: &Path, want: Want) -> bool {
        self.barren.contains(&(path.to_path_buf(), want))
    }

    /// Everything finished since the last look.
    ///
    /// Files that came back with nothing are recorded here rather than
    /// returned: there is nothing for the caller to do about them, and asking
    /// again next frame is the one thing that must not happen.
    pub fn take(&mut self) -> Vec<(PathBuf, Made)> {
        let mut out = Vec::new();
        loop {
            match self.done.try_recv() {
                Ok((job, made)) => {
                    self.asked.remove(&job);
                    match made {
                        Some(picture) => out.push((job.0, picture)),
                        None => {
                            self.barren.insert(job);
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The workers only stop if they cannot report, which they
                // cannot do while this end is held.
                Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}

/// One worker: take the most recently wanted picture and make it.
fn work(queue: &Queue, send: &Sender<(Job, Option<Made>)>) {
    loop {
        let job = {
            let Ok(mut jobs) = queue.jobs.lock() else {
                return;
            };
            loop {
                if let Some(job) = jobs.pop_front() {
                    break job;
                }
                let Ok(waited) = queue.ready.wait(jobs) else {
                    return;
                };
                jobs = waited;
            }
        };

        let made = produce(&job.0, job.1);
        if made.is_none() {
            tracing::debug!(
                file = %job.0.display(),
                want = ?job.1,
                "nothing here can make that picture of this file"
            );
        }
        if send.send((job, made)).is_err() {
            return;
        }
    }
}

/// Make one picture of a file.
fn produce(path: &Path, want: Want) -> Option<Made> {
    match want {
        Want::Thumbnail => thumbnail(path).map(Made::Thumbnail),
        Want::Backdrop => backdrop(path).map(Made::Backdrop),
        Want::Snapshot => snapshot(path).map(Made::Snapshot),
        Want::Logo => std::fs::read(path)
            .ok()
            .and_then(|bytes| crate::art::logo(&bytes))
            .map(Made::Logo),
    }
}

/// The cached thumbnail of `path`, or a freshly made one, which is then
/// cached.
fn thumbnail(path: &Path) -> Option<Picture> {
    let stamp = modified(path)?;
    if let Some(cached) = cached(path, stamp) {
        return Some(cached);
    }
    let made = render(path)?;
    store(path, stamp, &made);
    Some(made)
}

/// The picture behind the display, for one of the user's own photographs.
///
/// Only a photograph: [`Kind::Image`] and nothing else. A film's thumbnail is
/// one frame taken out of it by a tool that may not be installed, and a frame
/// of a film blown up across a display is neither what the file is nor
/// something anybody asked to look at.
///
/// Decoded again from the file rather than grown from the thumbnail, and read
/// through the same ceiling the thumbnailer uses: this is the user's own disk,
/// where a photograph straight off a camera is forty megapixels before it is
/// anything else.
fn backdrop(path: &Path) -> Option<crate::art::Scenery> {
    if crate::media::kind_of(path)? != Kind::Image {
        return None;
    }
    let image = full(path)?;
    Some(crate::art::scenery_from(image))
}

/// The picture behind the display, for a screenshot of one of somebody's own
/// games.
///
/// The same read as a backdrop's and a different reduction: what is being read
/// is a file this shell fetched into its own cache rather than one of the
/// user's photographs, and it is a few hundred pixels across. See
/// [`crate::art::blurred_scenery_from`], which is where the reason it is
/// softened rather than enlarged honestly is written down.
fn snapshot(path: &Path) -> Option<crate::art::Scenery> {
    if crate::media::kind_of(path)? != Kind::Image {
        return None;
    }
    let image = full(path)?;
    Some(crate::art::blurred_scenery_from(image))
}

/// One of the user's pictures, decoded whole.
///
/// A vector drawing is rendered instead, at the height the box behind the bar
/// is: a drawing has no pixels of its own to be decoded at, and rendering it at
/// the size it will be shown is the whole advantage of it being one.
fn full(path: &Path) -> Option<image::DynamicImage> {
    if is_svg(path) {
        let data = std::fs::read(path).ok()?;
        let rgba = crate::icons::rasterise_svg(&data, path.parent(), crate::art::HERO_HEIGHT)?;
        let side = crate::art::HERO_HEIGHT;
        return image::RgbaImage::from_raw(side, side, rgba).map(image::DynamicImage::ImageRgba8);
    }
    let data = std::fs::read(path).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

/// Whether a file is a vector drawing, which is rendered rather than decoded.
fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// When the source was last written, in whole seconds since the epoch — which
/// is the resolution the spec records it at, and therefore the resolution a
/// cached thumbnail can be checked against.
fn modified(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

/// Make a picture of a file that has none cached.
fn render(path: &Path) -> Option<Picture> {
    match crate::media::kind_of(path) {
        Some(Kind::Audio) => None,
        Some(Kind::Video) => from_film(path),
        Some(Kind::Image) => from_picture(path),
        // A file whose name says nothing about what is in it. Looked inside
        // rather than given up on, because the only files that reach here
        // unnamed are ones something has already decided are pictures — an
        // account's own portrait is `/var/lib/AccountsService/icons/<name>`,
        // with no extension at all, and the daemon that keeps it is the thing
        // saying it is a picture.
        //
        // It is not a widening of what gets thumbnailed. Nothing asks for a
        // picture of a file it has not already judged: the shelves ask by
        // [`Kind`], the file explorer asks only where [`crate::media::
        // has_picture`] agrees, and neither of those can produce a path with no
        // extension. What this adds is an answer for the one caller that can.
        //
        // [`decode`] sniffs the format from the bytes and fails quietly on
        // anything that is not a picture, so the worst this can cost is one
        // read of a file somebody pointed at.
        None => decode(&std::fs::read(path).ok()?),
    }
}

/// Scale an image file down. Vector drawings are rendered rather than decoded,
/// the way the shell's own glyphs are.
fn from_picture(path: &Path) -> Option<Picture> {
    if is_svg(path) {
        let data = std::fs::read(path).ok()?;
        let rgba = crate::icons::rasterise_svg(&data, path.parent(), SIZE)?;
        return Some(Picture {
            width: SIZE,
            height: SIZE,
            rgba,
        });
    }
    decode(&std::fs::read(path).ok()?)
}

/// Decode an image held in memory and scale it to fit [`SIZE`].
///
/// The decoder is given a ceiling, because the file is the user's and a
/// photograph straight off a camera is forty megapixels before it is anything
/// else. Without one, making a thumbnail of a folder of them would be the
/// largest allocation the shell ever makes, twice over, on a worker thread.
fn decode(data: &[u8]) -> Option<Picture> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);

    let image = reader.decode().ok()?;
    // Scaled down, never up. A picture smaller than a thumbnail is already
    // its own thumbnail, and blowing it up would cost four times the atlas to
    // store exactly the same detail, softened.
    let scaled = if image.width() > SIZE || image.height() > SIZE {
        image.resize(SIZE, SIZE, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let rgba = scaled.to_rgba8();
    Some(Picture {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// One frame out of a film, taken by whichever tool the machine has.
///
/// `ffmpegthumbnailer` first: it exists for exactly this, and it already knows
/// to skip past the black at the front and to pick a frame with something in
/// it. `ffmpeg` is the fallback, since a machine with any video player on it
/// almost certainly has it. Neither is a dependency — a session with neither
/// draws the film-strip glyph, which is what it drew before this existed.
fn from_film(path: &Path) -> Option<Picture> {
    let out = Scratch::new("png")?;
    let size = SIZE.to_string();

    let attempts: [(&str, Vec<&std::ffi::OsStr>); 3] = [
        (
            "ffmpegthumbnailer",
            argv(
                &["-i", "%f", "-o", "%o", "-s", &size, "-q", "8"],
                path,
                &out,
            ),
        ),
        (
            "ffmpeg",
            argv(
                &[
                    "-loglevel",
                    "error",
                    "-ss",
                    INTO_FILM,
                    "-i",
                    "%f",
                    "-frames:v",
                    "1",
                    "-y",
                    "%o",
                ],
                path,
                &out,
            ),
        ),
        // A film shorter than the seek is not a film that failed; it is a film
        // that ended before the frame we asked for.
        (
            "ffmpeg",
            argv(
                &[
                    "-loglevel",
                    "error",
                    "-i",
                    "%f",
                    "-frames:v",
                    "1",
                    "-y",
                    "%o",
                ],
                path,
                &out,
            ),
        ),
    ];

    for (program, args) in attempts {
        if !crate::model::executable_on_path(std::ffi::OsStr::new(program)) {
            continue;
        }
        if !ran(program, &args) {
            continue;
        }
        let Ok(data) = std::fs::read(&out.path) else {
            continue;
        };
        if let Some(picture) = decode(&data) {
            return Some(picture);
        }
    }
    None
}

/// Fill `%f` and `%o` in with the file being read and the file being written.
fn argv<'a>(template: &[&'a str], file: &'a Path, out: &'a Scratch) -> Vec<&'a std::ffi::OsStr> {
    template
        .iter()
        .map(|word| match *word {
            "%f" => file.as_os_str(),
            "%o" => out.path.as_os_str(),
            other => std::ffi::OsStr::new(other),
        })
        .collect()
}

/// Run a thumbnailer and say whether it finished, successfully, in time.
///
/// Killed rather than waited out: a tool wedged on a damaged file would
/// otherwise hold one of the two workers for the rest of the session, and the
/// row it was for is long off the screen.
fn ran(program: &str, args: &[&std::ffi::OsStr]) -> bool {
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(program, "thumbnailer took too long; giving up on it");
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A file to be written and then read back, removed whatever happens next.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(extension: &str) -> Option<Scratch> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let name = format!(
            "lxb-thumb-{}-{:?}.{extension}",
            std::process::id(),
            std::thread::current().id()
        );
        Some(Scratch {
            path: dir.join(name),
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// --- the shared cache ------------------------------------------------------

/// Where thumbnails of this size live, per the freedesktop specification.
fn cache_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(cache.join("thumbnails").join("large"))
}

/// The cache entry for a source file, whether or not it exists.
fn cache_file(path: &Path) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("{}.png", md5_hex(file_uri(path).as_bytes()))))
}

/// A thumbnail already on disk for this file, if it is still about this
/// version of it.
///
/// The recorded modification time is what makes that answerable. A photograph
/// edited in place keeps its name and its URI, so without the check the shell
/// would show a picture of what the file used to be — the one failure a cache
/// must not have.
fn cached(path: &Path, stamp: u64) -> Option<Picture> {
    let data = std::fs::read(cache_file(path)?).ok()?;
    if png_text(&data, "Thumb::MTime")? != stamp.to_string() {
        return None;
    }
    // The URI too, because the name is a digest and a digest can collide. It
    // costs one string comparison against a file that has already been read.
    if png_text(&data, "Thumb::URI").is_some_and(|uri| uri != file_uri(path)) {
        return None;
    }
    decode(&data)
}

/// Write a thumbnail where every other desktop will find it.
///
/// Into place by rename, so a reader — this shell in another session, or a
/// file manager — never sees a half-written PNG at a name that says it is
/// finished. Failures are logged and otherwise ignored: a cache that cannot be
/// written costs time on the next look and nothing else.
fn store(path: &Path, stamp: u64, picture: &Picture) {
    let Some(file) = cache_file(path) else {
        return;
    };
    let Some(dir) = file.parent() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::debug!(?err, ?dir, "cannot make the thumbnail cache directory");
        return;
    }

    let Some(png) = encode_png(
        picture,
        &[
            ("Thumb::URI", file_uri(path)),
            ("Thumb::MTime", stamp.to_string()),
        ],
    ) else {
        return;
    };

    let partial = dir.join(format!(
        ".lxb-{}-{:?}.png",
        std::process::id(),
        std::thread::current().id()
    ));
    let written = std::fs::File::create(&partial).and_then(|mut out| {
        // The spec asks for 0600: a thumbnail can be of anything the user has.
        out.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        out.write_all(&png)?;
        out.sync_all()
    });
    if let Err(err) = written {
        tracing::debug!(?err, "cannot write a thumbnail");
        let _ = std::fs::remove_file(&partial);
        return;
    }
    if let Err(err) = std::fs::rename(&partial, &file) {
        tracing::debug!(?err, "cannot put a thumbnail into the cache");
        let _ = std::fs::remove_file(&partial);
    }
}

/// A path as the URI the cache is keyed on.
///
/// Percent-encoded the way GLib encodes it, which is what the file managers
/// filling this cache use: everything unreserved, the sub-delimiters, `:` and
/// `@` are left alone and the rest — spaces, `#`, `?`, and every byte of a
/// non-ASCII name — is escaped. Getting this wrong costs no correctness and
/// all of the benefit: the digest simply would not match the one Dolphin wrote
/// beside it.
fn file_uri(path: &Path) -> String {
    format!("file://{}", percent_encoded(path))
}

/// The escaping half of that, on its own.
///
/// Shared with [`crate::trash`], which needs the same rule for a different
/// freedesktop file: a `.trashinfo` records where a file came from with the
/// path escaped exactly this way. Two encoders would be two chances to differ
/// over a byte, and the byte in question is always somebody's file name.
pub fn percent_encoded(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    const KEPT: &[u8] = b"-_.~!$&'()*+,;=:@/";
    let mut encoded = String::new();
    for byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || KEPT.contains(byte) {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

// --- PNG, as much of it as a cache entry needs -----------------------------

/// The text a PNG carries under `keyword`, if it carries any.
fn png_text(data: &[u8], keyword: &str) -> Option<String> {
    for (kind, body) in png_chunks(data) {
        if kind == b"tEXt" {
            let mut parts = body.splitn(2, |byte| *byte == 0);
            let key = parts.next()?;
            let value = parts.next()?;
            if key == keyword.as_bytes() {
                return String::from_utf8(value.to_vec()).ok();
            }
        }
        // Everything this reads is in the header; the pixels are not worth
        // walking past.
        if kind == b"IDAT" {
            break;
        }
    }
    None
}

/// The chunks of a PNG, as `(type, data)`, stopping at the first malformed
/// one.
fn png_chunks(data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    const SIGNATURE: usize = 8;
    let mut at = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        SIGNATURE
    } else {
        data.len()
    };
    std::iter::from_fn(move || {
        let length = u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind = data.get(at + 4..at + 8)?;
        let body = data.get(at + 8..at + 8 + length)?;
        at += 12 + length;
        Some((kind, body))
    })
}

/// A thumbnail as a PNG carrying the given text chunks.
///
/// The encoder writes the image and this puts the text in front of it: a
/// `tEXt` chunk is a length, a type, a NUL-separated key and value, and a
/// CRC, and it may sit anywhere after the header. Splicing it in is a great
/// deal less work than an encoder that could have been asked to write it.
fn encode_png(picture: &Picture, text: &[(&str, String)]) -> Option<Vec<u8>> {
    let mut png: Vec<u8> = Vec::new();
    image::write_buffer_with_format(
        &mut std::io::Cursor::new(&mut png),
        &picture.rgba,
        picture.width,
        picture.height,
        image::ExtendedColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .ok()?;

    // After the header, which is always the first chunk and is the only place
    // the length of what comes before can be read from.
    let header = u32::from_be_bytes(png.get(8..12)?.try_into().ok()?) as usize;
    let after = 8 + 12 + header;
    if after > png.len() {
        return None;
    }

    let mut chunks: Vec<u8> = Vec::new();
    for (keyword, value) in text {
        let mut body = Vec::with_capacity(keyword.len() + 1 + value.len());
        body.extend_from_slice(keyword.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());

        chunks.extend_from_slice(&(body.len() as u32).to_be_bytes());
        chunks.extend_from_slice(b"tEXt");
        chunks.extend_from_slice(&body);
        let mut crc = Vec::with_capacity(4 + body.len());
        crc.extend_from_slice(b"tEXt");
        crc.extend_from_slice(&body);
        chunks.extend_from_slice(&crc32(&crc).to_be_bytes());
    }

    png.splice(after..after, chunks);
    Some(png)
}

/// The CRC a PNG chunk carries: the standard reflected polynomial, over the
/// chunk's type and data.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let carry = crc & 1;
            crc >>= 1;
            if carry != 0 {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    !crc
}

// --- MD5, which is what the cache is keyed on ------------------------------

/// The MD5 of `data`, lower-case hex.
///
/// Not a security decision and not ours to make: the thumbnail specification
/// names MD5, every implementation of it uses MD5, and a different digest here
/// would mean a cache nobody else could read and that could not read anybody
/// else's.
fn md5_hex(data: &[u8]) -> String {
    const SHIFTS: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, //
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, //
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, //
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    // The round constants are defined as the integer part of the absolute sine
    // of the round number, scaled by 2^32 — written that way in the standard
    // rather than as a table, and computed the same way here.
    let sines: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32);

    let mut message = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_le_bytes());

    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in message.chunks_exact(64) {
        let words: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().expect("four bytes"))
        });
        let [mut a, mut b, mut c, mut d] = state;

        for i in 0..64 {
            let (mixed, word) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let turned = a
                .wrapping_add(mixed)
                .wrapping_add(sines[i])
                .wrapping_add(words[word]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(turned.rotate_left(SHIFTS[i]));
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut hex = String::with_capacity(32);
    for word in state {
        for byte in word.to_le_bytes() {
            hex.push_str(&format!("{byte:02x}"));
        }
    }
    hex
}

/// Whether anything on this machine can make a picture of a film.
///
/// Asked once, for the log and for the README's promise that a missing tool
/// costs exactly one feature.
pub fn films_can_be_thumbnailed() -> bool {
    ["ffmpegthumbnailer", "ffmpeg"]
        .iter()
        .any(|program| crate::model::executable_on_path(std::ffi::OsStr::new(program)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published vectors. A digest that is wrong by one bit is a cache
    /// nobody shares, which would look exactly like a cache that is merely
    /// cold.
    #[test]
    fn md5_matches_the_published_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        // Long enough to need a second block, and then a third.
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    /// The name a thumbnail is filed under: the digest and the URI it is taken
    /// over, which are the two things that have to agree with every other
    /// desktop for the cache to be shared at all.
    ///
    /// Both digests here were taken with `md5sum` on the command line rather
    /// than from this code, so the test is a comparison against something else
    /// rather than against itself.
    #[test]
    fn the_cache_is_named_the_way_every_other_desktop_names_it() {
        let path = Path::new("/home/jens/photo/me.png");
        assert_eq!(file_uri(path), "file:///home/jens/photo/me.png");
        assert_eq!(
            md5_hex(file_uri(path).as_bytes()),
            "d40775e596682f2a16d1b834c221c0a2"
        );
        assert_eq!(
            md5_hex(file_uri(Path::new("/home/x/a.png")).as_bytes()),
            "86848b816196d57cf98c8396af4b51ca"
        );
    }

    /// A music collection's worth of awkward names. Spaces and non-ASCII are
    /// escaped; the punctuation a URI allows is not.
    #[test]
    fn a_uri_escapes_what_it_must_and_leaves_the_rest() {
        assert_eq!(
            file_uri(Path::new("/home/x/My Photos/DSC 01.jpg")),
            "file:///home/x/My%20Photos/DSC%2001.jpg"
        );
        assert_eq!(
            file_uri(Path::new("/home/x/Bilder/Grünkohl.png")),
            "file:///home/x/Bilder/Gr%C3%BCnkohl.png"
        );
        assert_eq!(
            file_uri(Path::new("/home/x/a#b?c[d].png")),
            "file:///home/x/a%23b%3Fc%5Bd%5D.png"
        );
        assert_eq!(
            file_uri(Path::new("/home/x/Don't Stop (live).jpg")),
            "file:///home/x/Don't%20Stop%20(live).jpg"
        );
    }

    fn swatch(width: u32, height: u32) -> Picture {
        Picture {
            width,
            height,
            rgba: vec![200; (width * height * 4) as usize],
        }
    }

    /// What the cache is for: written by us, read back by anything that reads
    /// the specification — including us, next session.
    #[test]
    fn a_written_thumbnail_carries_what_says_it_is_still_good() {
        let png = encode_png(
            &swatch(4, 3),
            &[
                ("Thumb::URI", "file:///home/x/a.png".to_string()),
                ("Thumb::MTime", "1754700000".to_string()),
            ],
        )
        .expect("a PNG");

        assert_eq!(
            png_text(&png, "Thumb::MTime").as_deref(),
            Some("1754700000")
        );
        assert_eq!(
            png_text(&png, "Thumb::URI").as_deref(),
            Some("file:///home/x/a.png")
        );
        assert_eq!(png_text(&png, "Thumb::Size"), None);

        // And it is still a PNG that anything can decode, at the size it was
        // given — the text went in beside the image, not into it.
        let read = decode(&png).expect("a picture");
        assert_eq!((read.width, read.height), (4, 3));
    }

    /// The chunks have to be well formed or every other reader will reject the
    /// file: each carries its own length and its own CRC over type and body.
    #[test]
    fn the_spliced_chunks_are_well_formed() {
        let png = encode_png(&swatch(2, 2), &[("Thumb::MTime", "7".to_string())]).unwrap();
        let text: Vec<&[u8]> = png_chunks(&png)
            .filter(|(kind, _)| *kind == b"tEXt")
            .map(|(_, body)| body)
            .collect();
        assert_eq!(text.len(), 1);
        assert_eq!(text[0], b"Thumb::MTime\x007" as &[u8]);

        // Walked chunk by chunk, the file ends exactly where it ends: a length
        // written wrong would run the walk off the end or leave a tail.
        let walked: usize = png_chunks(&png).map(|(_, body)| body.len() + 12).sum();
        assert_eq!(walked + 8, png.len());
    }

    /// The CRC is the standard one; PNG's own end chunk is the fixed value
    /// every encoder writes.
    #[test]
    fn the_crc_is_the_one_png_specifies() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    /// A thumbnail is scaled to fit rather than cropped or stretched, so a
    /// portrait photograph stays portrait and the row can draw it at its own
    /// shape.
    #[test]
    fn a_picture_keeps_its_shape() {
        let wide = image::RgbaImage::from_pixel(1600, 900, image::Rgba([10, 20, 30, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(wide)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let made = decode(&png).expect("a picture");
        assert_eq!(made.width, SIZE);
        assert_eq!(made.height, SIZE * 9 / 16);
        assert_eq!(made.rgba.len(), (made.width * made.height * 4) as usize);
    }

    /// Asked for once, however many frames the row is on screen for, and never
    /// again once it is known there is nothing to be had.
    #[test]
    fn nothing_is_asked_for_twice() {
        let mut thumbs = Thumbs::start();
        let path = Path::new("/nonexistent/lxb-test/not-a-picture.png");

        thumbs.want(path, Want::Thumbnail);
        thumbs.want(path, Want::Thumbnail);
        assert_eq!(thumbs.asked.len(), 1);
        // At most one, because a worker may have taken it already — and the
        // whole point, that a second `want` did not put a second copy in. The
        // count alone cannot be asserted: the queue is drained by two threads
        // that started before this test did.
        let queued = thumbs.queue.jobs.lock().unwrap();
        assert!(
            queued.len() <= 1 && queued.iter().all(|(job, _)| job == path),
            "one job at most, and never a second copy of it: {queued:?}"
        );
        drop(queued);

        // The worker will find nothing there, and that answer sticks.
        let waited = std::time::Instant::now();
        while thumbs.barren.is_empty() && waited.elapsed().as_secs() < 5 {
            assert!(thumbs.take().is_empty(), "nothing can be made of it");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            thumbs.hopeless(path, Want::Thumbnail),
            "it came back with nothing"
        );
        assert!(thumbs.asked.is_empty());

        thumbs.want(path, Want::Thumbnail);
        assert!(thumbs.asked.is_empty(), "and it is not asked for again");
    }

    /// The two pictures of one file are two questions. A file that has no
    /// thumbnail is not thereby a file with no backdrop, and neither answer may
    /// be handed back as the other.
    #[test]
    fn a_thumbnail_and_a_backdrop_are_asked_for_separately() {
        let mut thumbs = Thumbs::start();
        let path = Path::new("/nonexistent/lxb-test/not-a-picture.png");

        thumbs.want(path, Want::Thumbnail);
        thumbs.want(path, Want::Backdrop);
        assert_eq!(thumbs.asked.len(), 2, "one job each, not one between them");

        let waited = std::time::Instant::now();
        while thumbs.barren.len() < 2 && waited.elapsed().as_secs() < 5 {
            assert!(thumbs.take().is_empty(), "nothing can be made of it");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(thumbs.hopeless(path, Want::Thumbnail));
        assert!(thumbs.hopeless(path, Want::Backdrop));
    }

    /// A photograph makes a backdrop; a film does not, whatever the thumbnailer
    /// on the machine could have made of it.
    #[test]
    fn only_a_photograph_is_made_into_a_backdrop() {
        let dir = std::env::temp_dir().join(format!("lxb-backdrop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("somewhere to write");
        let picture = dir.join("beach.jpg");
        let film = dir.join("clip.mp4");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            800,
            600,
            image::Rgb([10, 20, 30]),
        ))
        .save(&picture)
        .expect("a picture on the disk");
        std::fs::write(&film, b"not a film either").expect("a film's name");

        let made = backdrop(&picture).expect("a backdrop");
        assert_eq!(made.levels.len(), crate::art::HERO_LEVELS as usize);
        assert_eq!(
            made.levels[0].len(),
            (crate::art::HERO_WIDTH * crate::art::HERO_HEIGHT * 4) as usize,
            "the whole box, filled edge to edge"
        );
        assert!(
            backdrop(&film).is_none(),
            "a film has no picture to stand in"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file whose name says nothing about what is in it is looked inside.
    ///
    /// The one caller that produces such a path is an account's own portrait:
    /// `accounts-daemon` keeps it at `/var/lib/AccountsService/icons/<name>`,
    /// with no extension at all, and a thumbnailer that went by the name alone
    /// drew the plain figure for every account on the machine that has a
    /// picture. See [`render`].
    #[test]
    fn a_picture_with_no_extension_is_still_a_picture() {
        let dir = std::env::temp_dir().join(format!("lxb-portrait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("somewhere to write");
        // Named the way `accounts-daemon` names one: no extension, no dot.
        let portrait = dir.join("marta");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            256,
            256,
            image::Rgb([90, 40, 120]),
        ))
        .save_with_format(&portrait, image::ImageFormat::Png)
        .expect("a picture on the disk");

        assert_eq!(
            crate::media::kind_of(&portrait),
            None,
            "its name really does say nothing"
        );
        let made = render(&portrait).expect("the bytes say it is a PNG");
        assert!(made.width > 0 && made.height > 0);
        assert_eq!(made.rgba.len(), (made.width * made.height * 4) as usize);

        // And a file with no extension that is not a picture is still nothing,
        // rather than a decoder being asked to make something of it.
        let note = dir.join("README");
        std::fs::write(&note, b"just some text").expect("a file");
        assert!(render(&note).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
