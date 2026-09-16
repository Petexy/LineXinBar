//! Unpacking one of the user's archives: what is in the box, and where it goes.
//!
//! Every other row in a folder is opened by a program the user installed. An
//! archive is the one kind of file that is not a document at all — nobody wants
//! to *look* at a `.tar.gz`, they want what is inside it — so the shell answers
//! a press on one itself, under the name [`NAME`]. It is an application as far
//! as the rest of the machine is concerned: it has a desktop entry
//! ([`ENTRY`], shipped in `share/applications/`), it appears on the Open with
//! list like any other handler, and somebody who would rather press a `.zip`
//! and get Ark can say so there and have it stick — see [`crate::media`], which
//! is where "what opens this" is answered and where the shell's own answer is
//! the one that gives way.
//!
//! ## The shell does not know how to unpack anything
//!
//! It knows how to ask the thing that does, which is [`crate::uninstall`]'s
//! shape exactly: one family of archive becomes one argv, run on a thread,
//! reporting one outcome. Nothing here builds a shell command line and nothing
//! here decompresses a byte itself.
//!
//! Which program that is depends on what is installed, and the ladder per
//! family is in [`TOOLS`]. `bsdtar` — libarchive with a command line on it — is
//! first wherever it will do, because one program covers tar, zip, 7z, rar,
//! iso and cab, and because every distribution this shell is packaged for
//! already has it underneath its package manager. Where it is missing the
//! ladder falls to whatever else the machine has, and where the machine has
//! nothing at all the press is answered by a panel naming the package that
//! would fix it — the same answer a console with no core gets.
//!
//! ## Nothing lands on top of anything
//!
//! The rule the rest of the file manager keeps — see [`crate::transfer`] — and
//! here it is kept by never unpacking into the folder the user chose. The
//! archive is emptied into a hidden staging directory made inside that folder,
//! and only what comes out is moved into place:
//!
//! * one thing in the box, and that one thing takes a free name beside its
//!   neighbours — which is what makes `foo.tar.gz` holding a single `foo/`
//!   arrive as `foo/` and not as `foo/foo/`, and a bare `report.pdf.gz`
//!   arrive as a PDF rather than as a folder with a PDF in it;
//! * anything else, and the staging directory itself takes a free name — which
//!   is what stops a zip of two hundred loose files from emptying itself over
//!   somebody's Downloads.
//!
//! The staging directory is made *inside* the destination so that the move is a
//! rename on one filesystem: instant whatever the archive was the size of, and
//! never a half-unpacked folder standing under the name of a finished one. It
//! is removed whether the unpacking worked or not.
//!
//! ## And the other direction
//!
//! [`pack`] is the same bargain read backwards, for the Compress row of the
//! menu: one [`Format`] becomes one argv, run on a thread, reporting one
//! outcome, and the program writes into a hidden file beside where the archive
//! will stand that takes its real name only once the program has finished
//! with it. A half-written archive under a finished name is a file somebody
//! will open, and this shell never leaves one.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the shell's own handler is called, on the Open with list and anywhere
/// else a program is named.
pub const NAME: &str = "Extract";

/// The desktop entry that stands for it.
///
/// It is a real file on the disk — `share/applications/linexinbar-extract.desktop`
/// — for the reason `linexinbar-files.desktop` is one: the machine has to have
/// something to *name* as the handler for a type. A choice the user makes on
/// the Open with list is written into their `mimeapps.list` as a desktop entry
/// name and nothing else, so an Extract that had no name could be displaced and
/// never chosen back.
///
/// The shell recognises it by this name rather than by looking it up in the
/// catalogue, which is not a shortcut: the entry carries `NoDisplay=true`,
/// because there is no Extract application to put a tile on the bar for, and
/// the scan drops every one of those — so the catalogue never holds it on any
/// machine. Matching by name also makes the answer hold in a session running
/// straight out of a source tree, which has installed nothing anywhere. See
/// [`crate::media::is_the_extractor`].
pub const ENTRY: &str = "linexinbar-extract.desktop";

/// Everything Extract offers to open.
///
/// The types [`crate::files::described`] can actually produce for a file on
/// somebody's disk, and no more: a mime database this is not, and a row
/// claiming a type the explorer never reports would be a claim nothing could
/// ever test.
///
/// Deliberately **not** `.deb`, `.rpm` or `.AppImage`. Each of those is a
/// container the shell could empty, and none of them is a thing anybody presses
/// in order to get its contents out — they are programs, and answering a press
/// on one by scattering `usr/` across somebody's Downloads would be the shell
/// being clever instead of being right.
pub const OPENS: &[&str] = &[
    "application/zip",
    "application/x-tar",
    "application/gzip",
    "application/x-bzip2",
    "application/x-xz",
    "application/zstd",
    "application/x-lz4",
    "application/x-lzip",
    "application/x-lzma",
    "application/x-7z-compressed",
    "application/vnd.rar",
    "application/vnd.ms-cab-compressed",
    "application/x-cd-image",
];

/// Whether Extract is one of the things that open a file of this type.
pub fn opens(mime: &str) -> bool {
    OPENS.contains(&mime)
}

/// What kind of box this is, which is what decides the program and the command
/// line.
///
/// By the file's *name* and not by its type, because the two answer different
/// questions here: `holiday.tar.gz` and `disk.img.gz` are both
/// `application/gzip` to everything on the machine, and one of them is a folder
/// of photographs while the other is a single file with a coat on. What tells
/// them apart is the `.tar` in the middle, so that is what is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// A tar, plain or wearing any of the compressions.
    Tar,
    Zip,
    SevenZip,
    Rar,
    /// Everything else libarchive reads as a container of files — an ISO, a
    /// cabinet. One arm rather than one each, because the ladder and the
    /// command line are the same for all of them and only the sentence about a
    /// missing program would differ.
    Container,
    /// Not a container at all: one file with a compression wrapped round it.
    /// The program writes the file back out rather than writing into a folder,
    /// which is why it is a family of its own and not a tar without the tar.
    Compressed,
}

/// The whole of what unpacking one archive takes.
struct Plan {
    family: Family,
    /// The program, already found on `PATH`.
    program: &'static str,
    /// What the thing is called once it is out of its coat — the name a
    /// [`Family::Compressed`] file takes, and the name the staging directory
    /// falls back to for everything else. See [`unpacked_name`].
    unpacked: String,
}

/// Which programs can empty which kind of box, best first.
///
/// `bsdtar` heads every container ladder for one reason: it is libarchive, so
/// one program that is already on any machine with a package manager reads tar,
/// zip, 7z, rar, iso and cab alike, and a shell that reached for a different
/// tool per format would be a shell that works on the developer's machine.
/// What follows it is what somebody who has the format has probably installed
/// for it.
///
/// The two exceptions are the two formats with a canonical tool of their own.
/// `7z` reads its own containers better than libarchive does — solid blocks,
/// headers it wrote itself — and `unar` is what RAR is unpacked with on a
/// machine that will not have the non-free one.
const TOOLS: &[(Family, &[&str])] = &[
    (Family::Tar, &["bsdtar", "tar"]),
    (Family::Zip, &["bsdtar", "unzip", "7z", "7zz", "7za"]),
    (Family::SevenZip, &["7z", "7zz", "7za", "bsdtar"]),
    (Family::Rar, &["unar", "unrar", "7z", "7zz", "bsdtar"]),
    (Family::Container, &["bsdtar", "7z", "7zz", "7za"]),
];

/// The compressions a single file can be wearing, and what takes each one off.
///
/// The suffix is matched against the end of the lowercased name, so
/// `disk.img.zst` is a zstd file whatever `Path::extension` makes of it. Each
/// program is asked for the same two things — decompress, write to your own
/// output — which is why one line is enough to describe one.
const COATS: &[(&str, &str)] = &[
    (".gz", "gzip"),
    (".bz2", "bzip2"),
    (".xz", "xz"),
    (".lzma", "xz"),
    (".zst", "zstd"),
    (".lz4", "lz4"),
    (".lz", "lzip"),
];

/// The names a tar wears when its compression is folded into the extension.
const TARBALLS: &[&str] = &[".tgz", ".tbz", ".tbz2", ".txz", ".tzst", ".tlz"];

/// What family the file at `path` belongs to, by its name.
fn family(path: &Path) -> Option<Family> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let ends = |tail: &str| name.ends_with(tail);

    if ends(".tar") || TARBALLS.iter().any(|tail| ends(tail)) {
        return Some(Family::Tar);
    }
    // A coat over a tar is still a tar: the program that takes the coat off is
    // the one that reads the tar underneath, in one pass, without ever writing
    // the uncompressed archive to the disk.
    if let Some((coat, _)) = COATS.iter().find(|(coat, _)| ends(coat)) {
        let under = &name[..name.len() - coat.len()];
        return Some(if under.ends_with(".tar") {
            Family::Tar
        } else {
            Family::Compressed
        });
    }
    if ends(".zip") {
        return Some(Family::Zip);
    }
    if ends(".7z") {
        return Some(Family::SevenZip);
    }
    if ends(".rar") {
        return Some(Family::Rar);
    }
    if ends(".iso") || ends(".cab") {
        return Some(Family::Container);
    }
    None
}

/// What the thing inside is called: the name with its archive suffixes taken
/// off.
///
/// One coat, then the `.tar` under it, and nothing further: `linux.pkg.tar.zst`
/// comes out as `linux.pkg`, which is what it is, and `disk.img.gz` comes out
/// as `disk.img`, which is a file the machine can still recognise. Stripping
/// until nothing was left would turn the second into `disk` and lose the one
/// thing saying what it is.
fn unpacked_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut cut = name.len();
    if let Some(tail) = TARBALLS.iter().find(|tail| lower.ends_with(**tail)) {
        return name[..name.len() - tail.len()].to_string();
    }
    if let Some((coat, _)) = COATS.iter().find(|(coat, _)| lower.ends_with(*coat)) {
        cut -= coat.len();
    }
    for tail in [".tar", ".zip", ".7z", ".rar", ".iso", ".cab"] {
        if lower[..cut].ends_with(tail) {
            cut -= tail.len();
            break;
        }
    }
    // A name that is nothing but its own extension — `.gz` — keeps it, because
    // a file called nothing at all is not something to put on the disk.
    if cut == 0 {
        return name.to_string();
    }
    name[..cut].to_string()
}

/// Why a press on an archive cannot be answered, in a sentence for a panel.
///
/// The package rather than the program where the two differ, because what the
/// user is being asked to do is install something and `unar` is not what the
/// thing is called in half the repositories it is in. Naming one is the point:
/// "this cannot be unpacked" is a dead end, and this is a thing to go and do.
fn nothing_installed(family: Family) -> String {
    let (what, install) = match family {
        Family::Tar => ("tar archive", "libarchive"),
        Family::Zip => ("zip file", "libarchive"),
        Family::SevenZip => ("7z archive", "7zip"),
        Family::Rar => ("RAR archive", "unar"),
        Family::Container => ("file", "libarchive"),
        // Unreachable in practice — the coat and the program are one line of
        // [`COATS`] — but a compression whose tool has been removed from the
        // machine is still a sentence somebody has to be able to read.
        Family::Compressed => ("compressed file", "the program that made it"),
    };
    format!("Nothing on this machine unpacks a {what}. Install {install}.")
}

/// How the archive at `path` would be unpacked, or what to say about why it
/// cannot be.
fn plan(path: &Path) -> io::Result<Plan> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::other("that is not a file"))?;
    let family = family(path).ok_or_else(|| io::Error::other("this is not an archive"))?;
    let unpacked = unpacked_name(&name);

    let ladder: &[&str] = match family {
        Family::Compressed => {
            let lower = name.to_ascii_lowercase();
            match COATS.iter().find(|(coat, _)| lower.ends_with(*coat)) {
                Some((_, program)) => std::slice::from_ref(program),
                None => &[],
            }
        }
        family => TOOLS
            .iter()
            .find(|(kind, _)| *kind == family)
            .map_or(&[][..], |(_, ladder)| *ladder),
    };
    let program = ladder
        .iter()
        .find(|program| crate::model::executable_on_path(OsStr::new(**program)))
        .ok_or_else(|| io::Error::other(nothing_installed(family)))?;

    Ok(Plan {
        family,
        program,
        unpacked,
    })
}

/// Unpack `archive` into `into`, and answer with where its contents landed.
///
/// On the thread [`crate::transfer::Run`] gives it, which is why it is a plain
/// blocking function: an archive is as big as it is, and the shell draws at
/// sixty frames a second on the thread that would otherwise be doing this.
pub fn unpack(archive: &Path, into: &Path) -> io::Result<PathBuf> {
    let plan = plan(archive)?;
    let staging = make_staging(into)?;

    let ran = run(&plan, archive, &staging);
    if let Err(err) = ran {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }
    match promote(&staging, into, &plan.unpacked) {
        Ok(landed) => {
            // Empty by now for the single-thing case, and gone already for the
            // other. Either way nothing is left of it — a hidden directory
            // surviving a finished job would be litter in somebody's folder.
            let _ = std::fs::remove_dir_all(&staging);
            Ok(landed)
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(err)
        }
    }
}

/// A directory inside `into` that nothing else is using, for the archive to be
/// emptied into.
///
/// Hidden, and named after this process, so that the folder somebody is looking
/// at while the job runs does not grow a row they did not ask for and cannot
/// use. Inside `into` rather than under `/tmp` because what comes next is a
/// rename, and a rename only costs nothing when both ends are on one
/// filesystem — the alternative is copying a ten-gigabyte extraction twice.
fn make_staging(into: &Path) -> io::Result<PathBuf> {
    for attempt in 0..TRIES {
        let at = into.join(format!(".lxb-extract-{}-{attempt}", std::process::id()));
        match std::fs::create_dir(&at) {
            Ok(()) => return Ok(at),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("there was nowhere to unpack it"))
}

/// How many names are tried before giving up — [`crate::transfer::free_name`]'s
/// own number, for the reason it gives.
const TRIES: u32 = 100;

/// Run the program, and turn what it says about a failure into a sentence.
fn run(plan: &Plan, archive: &Path, staging: &Path) -> io::Result<()> {
    let mut command = Command::new(plan.program);
    command.stdin(Stdio::null()).stderr(Stdio::piped());

    match plan.family {
        // One file with a coat on: the program is told to write to its own
        // output, and its output is the file. A plain redirection of a file
        // descriptor and not a shell — there is no shell anywhere in this.
        Family::Compressed => {
            let out = std::fs::File::create(staging.join(&plan.unpacked))?;
            command
                .args(["-d", "-c"])
                .arg(archive)
                .stdout(Stdio::from(out));
        }
        _ => {
            // Kept rather than thrown away, because half of these tools say
            // what went wrong on their own output rather than on the error one:
            // unzip does, and so does 7-Zip. Nothing reads it while the job
            // runs — it is only ever looked at to write one sentence about a
            // failure. See [`complaint`].
            command.stdout(Stdio::piped());
            match plan.program {
                "bsdtar" | "tar" => {
                    command
                        .arg("-x")
                        .arg("-f")
                        .arg(archive)
                        .arg("-C")
                        .arg(staging);
                }
                // `-o` is not politeness: without it unzip stops to ask about a
                // name it has already written, on a terminal that is not there.
                "unzip" => {
                    command.args(["-qq", "-o", "-d"]).arg(staging).arg(archive);
                }
                "7z" | "7zz" | "7za" => {
                    command
                        .arg("x")
                        .arg("-y")
                        .arg(format!("-o{}", staging.display()))
                        .arg(archive);
                }
                "unar" => {
                    command.args(["-q", "-f", "-o"]).arg(staging).arg(archive);
                }
                // The one program that takes its destination as a bare path,
                // and it must end in a separator or it is read as a file to
                // write.
                "unrar" => {
                    command
                        .args(["x", "-y", "-idq"])
                        .arg(archive)
                        .arg(format!("{}/", staging.display()));
                }
                other => {
                    return Err(io::Error::other(format!("{other} was never taught to")));
                }
            }
        }
    }

    let output = command.output()?;
    if worked(plan.program, output.status.code()) {
        return Ok(());
    }
    Err(io::Error::other(complaint(plan.program, &output)))
}

/// Whether the program finished having done the job.
///
/// Zero everywhere, and one as well for `unzip` alone, which documents it as
/// *warnings, and the files are there* and keeps 2 and above for the failures
/// where nothing came out. That distinction is not a nicety: a zip written by
/// a Windows tool warns about half a dozen ordinary things, and reading those
/// as failures would delete a correct extraction and put a frightening sentence
/// on the screen about it.
///
/// It is `unzip` alone because it is the only one of these that says so. This
/// was written the other way round first, on the assumption that `bsdtar` uses
/// 1 for warnings too — it does not: it exits 0 when it trims a leading slash
/// off a member name and 1 when the archive is not an archive, so allowing 1
/// there made every unreadable file unpack silently into an empty folder.
fn worked(program: &str, code: Option<i32>) -> bool {
    matches!(code, Some(0)) || (program == "unzip" && code == Some(1))
}

/// What to put on a panel about a program that would not do it.
///
/// The program's own last word, because it is the only thing that knows — "Not
/// a valid archive", "Wrong password", "No space left on device" — and a shell
/// answering every one of those with "it did not unpack" is a shell nobody can
/// act on. The *last* line rather than the first: every one of these tools
/// prints its progress complaints first and its verdict last.
fn complaint(program: &str, output: &std::process::Output) -> String {
    // The error output first and its own output after it, because the tools
    // disagree about which one a complaint belongs on — unzip and 7-Zip put
    // theirs on the ordinary one — and a shell that read only the conventional
    // half would answer half of these with nothing.
    let said = String::from_utf8_lossy(&output.stderr);
    let also = String::from_utf8_lossy(&output.stdout);
    let last = said
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .or_else(|| also.lines().map(str::trim).rfind(|line| !line.is_empty()));
    let Some(line) = last else {
        return match output.status.code() {
            Some(code) => format!("{program} gave up ({code})"),
            None => format!("{program} was stopped"),
        };
    };
    // Its own name off the front. Every one of these prefixes what it says with
    // what it is called, which is right in a terminal and noise on a panel: the
    // user pressed an archive and never chose `bsdtar`, and the sentence is
    // capitalised where it is shown — see [`crate::transfer::said`] — so the
    // name would come back as "Bsdtar" as well as being beside the point.
    line.strip_prefix(&format!("{program}: "))
        .unwrap_or(line)
        .to_string()
}

/// Move what came out of the staging directory into the folder the user chose,
/// and answer with where it went.
///
/// The one thing in the box is lifted out under its own name; anything else
/// keeps the box, under the archive's. Which of the two it is is read off the
/// disk rather than out of a listing of the archive: the tools disagree about
/// how to list one, several of them cannot be asked at all without unpacking
/// it, and what is on the disk afterwards is the only answer that is certainly
/// true.
fn promote(staging: &Path, into: &Path, unpacked: &str) -> io::Result<PathBuf> {
    let mut inside = std::fs::read_dir(staging)?.collect::<io::Result<Vec<_>>>()?;
    if inside.len() == 1 {
        let only = inside.remove(0);
        let landing = free(into, &only.file_name().to_string_lossy())?;
        std::fs::rename(only.path(), &landing)?;
        return Ok(landing);
    }
    let landing = free(into, unpacked)?;
    std::fs::rename(staging, &landing)?;
    Ok(landing)
}

/// A name in `into` that nothing is using: the one asked for, or the one
/// [`crate::transfer::free_name`] makes out of it.
///
/// Asked without following links, for the reason a transfer asks it that way: a
/// symbolic link pointing at somewhere that has gone is a name that is taken
/// all the same, and writing "through" it would put the contents wherever the
/// link happened to point.
fn free(into: &Path, name: &str) -> io::Result<PathBuf> {
    let wanted = into.join(name);
    if std::fs::symlink_metadata(&wanted).is_err() {
        return Ok(wanted);
    }
    crate::transfer::free_name(into, name)
}

// --- making one -----------------------------------------------------------

/// The kinds of archive the Compress row offers to make, in the order the list
/// offers them.
///
/// The ones anybody asks for by name, and no more. Zip first because it is the
/// one every other machine opens; the tars after it in the order of how often
/// each compression is reached for; 7z and RAR last because each wants a
/// program of its own that a machine may not have — see [`Format::writers`] —
/// so they are the two most likely to be missing from a list, and a list that
/// loses its last rows keeps its shape.
///
/// No bare `.gz` of one file: it is a format nobody chooses from a list — a
/// person who wants one has a reason, and a shell that offered it beside zip
/// would be offering a coat with nothing to put it on when the set is two
/// files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    SevenZip,
    Rar,
}

/// Every kind, in the order the list shows them.
pub const FORMATS: &[Format] = &[
    Format::Zip,
    Format::Tar,
    Format::TarGz,
    Format::TarXz,
    Format::TarZst,
    Format::TarBz2,
    Format::SevenZip,
    Format::Rar,
];

impl Format {
    /// What it is called on the list, which is also what the name ends in.
    ///
    /// The suffix and nothing else — "tar.gz" rather than "Gzipped tar" —
    /// because the suffix is the one name for the format that every person
    /// who has ever met one knows, and it is what will be written after the
    /// name they are typing.
    pub fn suffix(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::TarXz => "tar.xz",
            Format::TarZst => "tar.zst",
            Format::TarBz2 => "tar.bz2",
            Format::SevenZip => "7z",
            Format::Rar => "rar",
        }
    }

    /// The file `name` would be called as one of these.
    pub fn named(self, name: &str) -> String {
        format!("{name}.{}", self.suffix())
    }

    /// Which programs write it, best first.
    ///
    /// `bsdtar` heads every ladder it can, for the reason it heads the
    /// unpacking ones: one program already on any machine with a package
    /// manager writes tar in every coat, zip and 7z alike, told which by the
    /// name of the file it is asked to write (`-a`). GNU `tar` reads the same
    /// switch and follows it for the tars. `zip` and 7-Zip are what somebody
    /// who has no libarchive has instead, and RAR is written by the one
    /// program that can — there is no free one, and a ladder of one rung is
    /// the honest length.
    fn writers(self) -> &'static [&'static str] {
        match self {
            Format::Zip => &["bsdtar", "zip", "7z", "7zz", "7za"],
            Format::Tar | Format::TarGz | Format::TarXz | Format::TarZst | Format::TarBz2 => {
                &["bsdtar", "tar"]
            }
            Format::SevenZip => &["7z", "7zz", "7za", "bsdtar"],
            Format::Rar => &["rar"],
        }
    }

    /// The program on this machine that writes it, if there is one.
    pub fn writer(self) -> Option<&'static str> {
        self.writers()
            .iter()
            .copied()
            .find(|program| crate::model::executable_on_path(OsStr::new(program)))
    }
}

/// Every kind this machine can make, in the list's order.
///
/// Asked on the press that raises the Compress panel and not before: it is a
/// handful of `stat`s down `PATH`, and what is installed can change between
/// one press and the next. A kind nothing here writes is left off the list
/// rather than greyed on it — a greyed row is for something the user could
/// light from where they are standing, and installing RAR is not that.
pub fn writable() -> Vec<Format> {
    FORMATS
        .iter()
        .copied()
        .filter(|format| format.writer().is_some())
        .collect()
}

/// What to say when nothing on the machine makes an archive of any kind at
/// all, in a sentence for a panel.
///
/// libarchive, because it is the one package that answers every row of the
/// list but the last — the same answer [`nothing_installed`] gives for a tar.
pub fn nothing_writes() -> String {
    "Nothing on this machine makes an archive. Install libarchive.".to_string()
}

/// Make an archive of `members` — every one of them standing in `from` — as
/// `into/<name>.<suffix>`, and answer with where it landed.
///
/// On the thread [`crate::transfer::Run`] gives it, for the reason
/// [`unpack`] is blocking. The members are given by name and the folder once,
/// rather than as paths, because that is what every one of these programs
/// wants: told `-C from a b c`, or run in `from` and told `a b c`, it stores
/// `a`, `b` and `c` at the top of the archive, and what comes out again lands
/// beside itself. Handed absolute paths it would store the whole path from
/// the root, which is an archive that unpacks into `home/somebody/...`.
///
/// The name is not checked here for being free. The panel that asks for it
/// has already refused a taken one — see `Compressing::fault_in_name` — and what
/// this does with a name that was taken in between is what [`promote`] does:
/// take the next free one rather than write over anything.
pub fn pack(
    from: &Path,
    members: &[String],
    into: &Path,
    name: &str,
    format: Format,
) -> io::Result<PathBuf> {
    if members.is_empty() {
        return Err(io::Error::other("there is nothing to put in it"));
    }
    let program = format
        .writer()
        .ok_or_else(|| io::Error::other(nothing_writes()))?;
    let staging = staging_file(into, format)?;

    let written = write(program, format, from, members, &staging);
    if let Err(err) = written {
        let _ = std::fs::remove_file(&staging);
        return Err(err);
    }
    let landing = match free_archive(into, name, format) {
        Ok(landing) => landing,
        Err(err) => {
            let _ = std::fs::remove_file(&staging);
            return Err(err);
        }
    };
    if let Err(err) = std::fs::rename(&staging, &landing) {
        let _ = std::fs::remove_file(&staging);
        return Err(err);
    }
    Ok(landing)
}

/// A name in `into` for an archive called `name`, of `format`, that nothing is
/// using: `holiday.tar.gz`, or `holiday (2).tar.gz`.
///
/// Not [`crate::transfer::free_name`], which numbers a name it was handed
/// whole and puts the number before whatever `Path::extension` answers — the
/// last piece, so a tarball would come out as `holiday.tar (2).gz`, which
/// [`family`] then reads as one file in a coat. Here the name and the suffix
/// are known apart, so the number goes between them and the second archive
/// is the same kind as the first.
fn free_archive(into: &Path, name: &str, format: Format) -> io::Result<PathBuf> {
    let wanted = into.join(format.named(name));
    if std::fs::symlink_metadata(&wanted).is_err() {
        return Ok(wanted);
    }
    for number in 2..TRIES + 2 {
        let candidate = into.join(format.named(&format!("{name} ({number})")));
        if std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "there is no free name left in that folder",
    ))
}

/// A hidden file in `into` that nothing else is using, for the program to
/// write into, wearing the suffix the finished archive will wear.
///
/// The suffix is not decoration: `bsdtar -a` and `tar -a` read the format off
/// the name of the file they are asked to write, so a staging file called
/// anything else would come out as a plain tar whatever was chosen. Hidden and
/// named after this process for the reason [`make_staging`] is, and in `into`
/// for the same reason too — what comes next is a rename.
///
/// Made with `create_new`, so the name is claimed before the program starts
/// rather than found free and then raced for.
fn staging_file(into: &Path, format: Format) -> io::Result<PathBuf> {
    for attempt in 0..TRIES {
        let at = into.join(format!(
            ".lxb-compress-{}-{attempt}.{}",
            std::process::id(),
            format.suffix()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&at)
        {
            Ok(_) => return Ok(at),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("there was nowhere to write it"))
}

/// Run the program that writes `format`, and turn what it says about a
/// failure into a sentence.
///
/// Every program is told the members by name and the folder they are in, and
/// the two halves of the list differ only in *how*: the tars take `-C`, and
/// the rest are run with that folder as their working directory, which means
/// the same thing to them. The output is named in full, so the working
/// directory changes nothing about where it goes.
fn write(
    program: &str,
    format: Format,
    from: &Path,
    members: &[String],
    out: &Path,
) -> io::Result<()> {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match program {
        // `-a` is the whole of the format: the program reads it off the
        // suffix of `out`, which is why the staging file wears one. The
        // members are listed after `-C`, so each is stored under its own
        // name.
        "bsdtar" | "tar" => {
            command
                .args(["-c", "-a", "-f"])
                .arg(out)
                .arg("-C")
                .arg(from)
                .args(members);
        }
        // `-r` walks into a folder; `-q` keeps the listing of every member off
        // the output, which is not being read.
        "zip" => {
            command
                .current_dir(from)
                .args(["-r", "-q"])
                .arg(out)
                .args(members);
        }
        // Told the type outright rather than left to read the name, because
        // 7-Zip's own default is 7z whatever the file is called and the list
        // reaches it for zip as well. `-y` is the same `-y` unpacking passes.
        "7z" | "7zz" | "7za" => {
            let kind = match format {
                Format::Zip => "-tzip",
                _ => "-t7z",
            };
            command
                .current_dir(from)
                .args(["a", "-y", kind])
                .arg(out)
                .args(members);
        }
        // `-r` walks into a folder, and `-idq` keeps its progress off the
        // output — the same two switches unpacking with `unrar` passes, from
        // the other end.
        "rar" => {
            command
                .current_dir(from)
                .args(["a", "-r", "-idq"])
                .arg(out)
                .args(members);
        }
        other => {
            return Err(io::Error::other(format!("{other} was never taught to")));
        }
    }
    let output = command.output()?;
    if worked(program, output.status.code()) {
        return Ok(());
    }
    Err(io::Error::other(complaint(program, &output)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coat_over_a_tar_is_still_a_tar() {
        assert_eq!(family(Path::new("/x/holiday.tar.gz")), Some(Family::Tar));
        assert_eq!(family(Path::new("/x/holiday.TAR.ZST")), Some(Family::Tar));
        assert_eq!(family(Path::new("/x/holiday.tgz")), Some(Family::Tar));
        assert_eq!(family(Path::new("/x/holiday.tar")), Some(Family::Tar));
        // And the same compression over something that is not one is a single
        // file with a coat on, which is unpacked a different way entirely.
        assert_eq!(
            family(Path::new("/x/disk.img.gz")),
            Some(Family::Compressed)
        );
        assert_eq!(
            family(Path::new("/x/notes.txt.xz")),
            Some(Family::Compressed)
        );
    }

    #[test]
    fn the_containers_are_told_apart() {
        assert_eq!(family(Path::new("/x/a.zip")), Some(Family::Zip));
        assert_eq!(family(Path::new("/x/a.7z")), Some(Family::SevenZip));
        assert_eq!(family(Path::new("/x/a.RaR")), Some(Family::Rar));
        assert_eq!(family(Path::new("/x/a.iso")), Some(Family::Container));
        // And a file nothing here knows is not an archive, which is what lets
        // the press fall through to whatever does open it.
        assert_eq!(family(Path::new("/x/a.flac")), None);
        assert_eq!(family(Path::new("/x/a")), None);
    }

    /// One coat and the `.tar` under it, and no further: what is left has to go
    /// on saying what the thing is.
    #[test]
    fn the_name_inside_keeps_what_says_what_it_is() {
        assert_eq!(unpacked_name("holiday.tar.gz"), "holiday");
        assert_eq!(unpacked_name("holiday.tgz"), "holiday");
        assert_eq!(unpacked_name("linux.pkg.tar.zst"), "linux.pkg");
        assert_eq!(unpacked_name("disk.img.gz"), "disk.img");
        assert_eq!(unpacked_name("photos.zip"), "photos");
        assert_eq!(unpacked_name("photos.RAR"), "photos");
        // A name that is nothing but its own suffix keeps it: a file called
        // nothing at all is not something to put on somebody's disk.
        assert_eq!(unpacked_name(".gz"), ".gz");
    }

    /// The list Extract claims and the table the explorer names files out of
    /// have to agree, or the shell offers to unpack a type it can never be
    /// handed.
    #[test]
    fn everything_extract_claims_is_a_type_the_explorer_reports() {
        for mime in OPENS {
            assert!(
                crate::files::names_a_type(mime),
                "{mime} is claimed by Extract and no file is ever described as one"
            );
        }
    }

    /// A directory of this test's own under the system's temporary folder, or
    /// `None` where there is nowhere to write — the explorer's own tests do the
    /// same, for the same reason.
    fn scratch(name: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lxb-archive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// The whole journey, against a real archive made by whatever this machine
    /// has: a `.tar.gz` holding one folder arrives as that folder and not as a
    /// folder with a folder in it.
    #[test]
    fn one_thing_in_the_box_is_lifted_out_of_it() {
        let Some(dir) = scratch("one") else {
            return;
        };
        let made = dir.join("made");
        std::fs::create_dir_all(made.join("album")).unwrap();
        std::fs::write(made.join("album/track.txt"), b"x").unwrap();
        if !crate::model::executable_on_path(OsStr::new("bsdtar")) {
            return;
        }
        let archive = dir.join("album.tar.gz");
        let packed = Command::new("bsdtar")
            .arg("-c")
            .arg("-z")
            .arg("-f")
            .arg(&archive)
            .arg("-C")
            .arg(&made)
            .arg("album")
            .status()
            .unwrap();
        assert!(packed.success());

        let into = dir.join("into");
        std::fs::create_dir_all(&into).unwrap();
        let landed = unpack(&archive, &into).unwrap();
        assert_eq!(landed, into.join("album"));
        assert!(landed.join("track.txt").is_file());

        // And again, over the top of itself: nothing is overwritten, so the
        // second one lands beside the first.
        let again = unpack(&archive, &into).unwrap();
        assert_eq!(again, into.join("album (2)"));
        assert!(into.join("album/track.txt").is_file());

        // Nothing of the staging is left behind either way.
        let litter: Vec<_> = std::fs::read_dir(&into)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".lxb-extract"))
            .collect();
        assert!(litter.is_empty(), "left {litter:?} behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And a box of loose files keeps the box, under the archive's own name,
    /// rather than emptying itself over the folder it was pressed in.
    #[test]
    fn loose_files_keep_the_box() {
        let Some(dir) = scratch("loose") else {
            return;
        };
        if !crate::model::executable_on_path(OsStr::new("bsdtar")) {
            return;
        }
        let made = dir.join("made");
        std::fs::create_dir_all(&made).unwrap();
        std::fs::write(made.join("one.txt"), b"1").unwrap();
        std::fs::write(made.join("two.txt"), b"2").unwrap();
        let archive = dir.join("pair.tar");
        assert!(Command::new("bsdtar")
            .arg("-c")
            .arg("-f")
            .arg(&archive)
            .arg("-C")
            .arg(&made)
            .arg("one.txt")
            .arg("two.txt")
            .status()
            .unwrap()
            .success());

        let into = dir.join("into");
        std::fs::create_dir_all(&into).unwrap();
        let landed = unpack(&archive, &into).unwrap();
        assert_eq!(landed, into.join("pair"));
        assert!(landed.join("one.txt").is_file());
        assert!(landed.join("two.txt").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only `unzip` documents a middle exit status, and reading one into
    /// `bsdtar` made every unreadable file unpack silently into an empty
    /// folder — it exits 0 when it trims a leading slash and 1 when the archive
    /// is not an archive.
    #[test]
    fn a_warning_is_success_only_where_the_tool_says_so() {
        assert!(worked("bsdtar", Some(0)));
        assert!(!worked("bsdtar", Some(1)));
        assert!(!worked("tar", Some(1)));
        assert!(worked("unzip", Some(0)));
        assert!(worked("unzip", Some(1)));
        assert!(!worked("unzip", Some(9)));
        // Killed by a signal, which has no code at all.
        assert!(!worked("bsdtar", None));
    }

    /// And the same thing end to end: a file wearing an archive's name that is
    /// not one comes back as a failure, with the program's own words and
    /// without its name on the front of them.
    #[test]
    fn a_file_that_is_not_what_it_claims_is_a_failure() {
        let Some(dir) = scratch("claims") else {
            return;
        };
        if !crate::model::executable_on_path(OsStr::new("bsdtar")) {
            return;
        }
        let pretending = dir.join("photos.tar.gz");
        std::fs::write(&pretending, b"not an archive at all").unwrap();
        let err = unpack(&pretending, &dir).unwrap_err().to_string();
        assert!(!err.is_empty());
        assert!(!err.starts_with("bsdtar:"), "{err}");
        // Nothing was made, and nothing was left behind.
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["photos.tar.gz"], "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every kind on the list ends the name it makes in the word the list
    /// shows for it, and unpacking knows every one of those names as an
    /// archive — a kind the shell could make and then not open would be a
    /// box with no way back out of it.
    #[test]
    fn everything_compress_makes_is_something_extract_opens() {
        for format in FORMATS {
            let made = format.named("holiday");
            assert!(made.ends_with(format.suffix()), "{made}");
            assert!(
                family(Path::new(&made)).is_some(),
                "{made} would be made and then not be an archive"
            );
        }
        assert_eq!(Format::TarGz.named("holiday"), "holiday.tar.gz");
        assert_eq!(Format::Zip.named("a.b"), "a.b.zip");
    }

    /// The list only ever offers what the machine can write, in the list's
    /// own order, and whatever is offered has a program behind it.
    #[test]
    fn only_what_the_machine_writes_is_offered() {
        let offered = writable();
        for format in &offered {
            assert!(format.writer().is_some());
        }
        let order: Vec<usize> = offered
            .iter()
            .map(|format| FORMATS.iter().position(|known| known == format).unwrap())
            .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// The whole journey the other way: a folder and a file beside it go into
    /// one archive under the name asked for, the members are stored under
    /// their own names, nothing of the staging is left, and a second archive
    /// under the same name lands beside the first rather than on it.
    #[test]
    fn a_set_goes_into_one_box_under_its_own_names() {
        let Some(dir) = scratch("pack") else {
            return;
        };
        if !crate::model::executable_on_path(OsStr::new("bsdtar")) {
            return;
        }
        let from = dir.join("from");
        std::fs::create_dir_all(from.join("album")).unwrap();
        std::fs::write(from.join("album/track.txt"), b"x").unwrap();
        std::fs::write(from.join("notes.txt"), b"y").unwrap();
        let members = vec!["album".to_string(), "notes.txt".to_string()];

        let landed = pack(&from, &members, &from, "holiday", Format::TarGz).unwrap();
        assert_eq!(landed, from.join("holiday.tar.gz"));
        assert!(landed.is_file());
        let again = pack(&from, &members, &from, "holiday", Format::Zip).unwrap();
        assert_eq!(again, from.join("holiday.zip"));

        // Unpacked, what comes out is the two things under their own names —
        // not `from/album`, and not the archive's own path.
        let into = dir.join("into");
        std::fs::create_dir_all(&into).unwrap();
        let out = unpack(&landed, &into).unwrap();
        assert_eq!(out, into.join("holiday"));
        assert!(out.join("album/track.txt").is_file());
        assert!(out.join("notes.txt").is_file());

        // Nothing is written over: the same name again lands beside it.
        let beside = pack(&from, &members, &from, "holiday", Format::TarGz).unwrap();
        assert_eq!(beside, from.join("holiday (2).tar.gz"));

        let litter: Vec<_> = std::fs::read_dir(&from)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".lxb-compress"))
            .collect();
        assert!(litter.is_empty(), "left {litter:?} behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A member that is not there is a failure in the program's own words,
    /// and the half-written file does not survive it under any name.
    #[test]
    fn a_member_that_is_missing_leaves_nothing_behind() {
        let Some(dir) = scratch("missing") else {
            return;
        };
        if !crate::model::executable_on_path(OsStr::new("bsdtar")) {
            return;
        }
        let err = pack(&dir, &["nothing.txt".to_string()], &dir, "box", Format::Zip).unwrap_err();
        assert!(!err.to_string().is_empty());
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not an archive at all is refused before anything is made
    /// on the disk, and the sentence says so.
    #[test]
    fn something_that_is_not_an_archive_is_refused() {
        let Some(dir) = scratch("refused") else {
            return;
        };
        let not = dir.join("song.flac");
        std::fs::write(&not, b"x").unwrap();
        let err = unpack(&not, &dir).unwrap_err();
        assert!(err.to_string().contains("not an archive"));
        // And nothing was made to hold it.
        let litter: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".lxb-extract"))
            .collect();
        assert!(litter.is_empty(), "left {litter:?} behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
