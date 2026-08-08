//! What installed an application, and what that installation says about it.
//!
//! Two questions with one answer. "Which version of this is on the machine and
//! how much of the disk does it take" and "how would this be removed" are both
//! answered by whatever put it there, so finding that out is done once, here,
//! and [`crate::uninstall`] builds on the same [`Origin`].
//!
//! Neither fact is in the desktop entry. `.desktop` files carry a `Version` key
//! and it is the version of the *specification* the file is written to, not of
//! the program — an entry saying `Version=1.5` is claiming to be a 2017-era
//! desktop file, and showing that to a user as the application's version would
//! be a confident lie.
//!
//! ## What is recognised, and what happens to the rest
//!
//! Flatpak and Snap are recognised from where their desktop entry lives, which
//! is exact: both export into a directory of their own. Everything else is
//! offered to each system package manager in turn and the first that claims the
//! file wins, so the same code answers on a distribution nobody involved has
//! ever run.
//!
//! The list of managers is deliberately short — pacman, dpkg and rpm, which is
//! most of the installed Linux world — and anything else comes back
//! [`Origin::Unknown`]. That is not an oversight. `Unknown` is a *safe* answer:
//! the shell says it cannot tell what installed the application and offers
//! nothing further. Guessing at the output format of a tool nobody here has run
//! would be fine for the version row and dangerous for the command that deletes
//! software, and the two share this code. Adding a manager is one variant of
//! [`Manager`], one owner query, one facts query and one removal recipe, and
//! whoever adds it will have the tool in front of them.
//!
//! ## Why it is on a thread
//!
//! Asking costs two processes and something like a tenth of a second: `pacman
//! -Qo` on a warm cache takes 80 ms and `pacman -Qi` another 60. That is eight
//! dropped frames on the one press the user is watching most closely, so the
//! question is asked on a worker and the panel is drawn with the answer missing
//! until it lands — the same arrangement, and for the same reason, as
//! [`crate::system`]'s bars.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a package manager is given before the shell stops waiting for it.
///
/// Generous, because being slow is normal — an `rpm` database on a cold cache
/// is not quick — and finite because the alternative to giving up is a panel
/// that says "Reading…" for the rest of the session. Nothing is killed when it
/// expires: the worker is left to finish into a slot nobody reads any more,
/// which costs one thread and no correctness.
const PATIENCE: Duration = Duration::from_secs(4);

/// A packaging system this shell knows how to read and how to remove from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Pacman,
    Dpkg,
    Rpm,
}

/// Which flatpak installation an application is in.
///
/// The distinction is the whole of why it matters: a user installation belongs
/// to the person logged in and comes out again with no authority at all, while
/// a system one is shared and does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    System,
}

/// What put an application on the machine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Origin {
    Flatpak {
        id: String,
        scope: Scope,
    },
    Snap {
        name: String,
    },
    System {
        manager: Manager,
        package: String,
    },
    /// Nothing here claims it: a tarball unpacked into `~/.local`, a build from
    /// source, an AppImage, or a packaging system this shell has not been
    /// taught. Carried as an answer in its own right rather than as an absence,
    /// because "we do not know" is exactly what the user has to be told before
    /// being offered a button that deletes things.
    #[default]
    Unknown,
}

impl Origin {
    /// What to call the thing that would actually be removed — the package
    /// name, not the application's own. They differ often enough to be worth
    /// showing: `nm-connection-editor` installs "Advanced Network
    /// Configuration".
    pub fn package(&self) -> Option<&str> {
        match self {
            Origin::Flatpak { id, .. } => Some(id),
            Origin::Snap { name } => Some(name),
            Origin::System { package, .. } => Some(package),
            Origin::Unknown => None,
        }
    }
}

/// What the installation says about the application.
///
/// Every field is optional because every field can genuinely be unknown, and
/// saying so is the truth in that case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub version: Option<String>,
    /// Installed size in bytes, as the package manager reports it.
    pub size: Option<u64>,
}

/// Everything one enquiry comes back with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub origin: Origin,
    pub facts: Facts,
}

/// A question being put to the package manager.
///
/// Started when the panel that wants it opens and read once a frame after that.
/// It is deliberately not cached between openings: a package can be upgraded or
/// removed while the shell is running, and the whole point is that it says what
/// is installed *now*.
pub struct Lookup {
    answer: Arc<Mutex<Option<Answer>>>,
    asked: Instant,
}

impl Lookup {
    /// Ask about the application installed by the desktop entry at `path`.
    pub fn start(path: &Path) -> Self {
        let answer = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&answer);
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let origin = identify(&path);
            let found = Answer {
                facts: facts(&origin),
                origin,
            };
            tracing::debug!(?path, ?found, "package manager answered");
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(found);
            }
        });
        Self {
            answer,
            asked: Instant::now(),
        }
    }

    /// The answer, or `None` while the package manager is still being asked.
    ///
    /// Once [`PATIENCE`] has run out this is the empty answer rather than
    /// `None`, so a caller that draws "Reading…" until this returns something
    /// cannot be left drawing it for ever.
    pub fn answer(&self) -> Option<Answer> {
        match self.answer.lock() {
            Ok(slot) => slot.clone(),
            // A worker that panicked mid-answer is a worker that will never
            // answer; that is the same situation as running out of patience.
            Err(_) => Some(Answer::default()),
        }
        .or_else(|| (self.asked.elapsed() >= PATIENCE).then(Answer::default))
    }
}

/// Work out what put the application at `path` on the machine.
pub fn identify(path: &Path) -> Origin {
    // The two that are recognised from where they live come first, and have to:
    // a flatpak's desktop entry is exported out of the installation's own
    // directory, so a system package manager either does not know it or — worse
    // — traces it back to the `flatpak` package itself and would then report,
    // and offer to remove, the *tool* rather than the application.
    if let Some(id) = flatpak_id(path) {
        if let Some(scope) = flatpak_scope(&id) {
            return Origin::Flatpak { id, scope };
        }
    }
    if let Some(name) = snap_name(path) {
        return Origin::Snap { name };
    }
    for (manager, owner) in [
        (Manager::Pacman, pacman_owner as fn(&Path) -> Option<String>),
        (Manager::Dpkg, dpkg_owner),
        (Manager::Rpm, rpm_owner),
    ] {
        if let Some(package) = owner(path) {
            return Origin::System { manager, package };
        }
    }
    Origin::Unknown
}

/// Ask the installation what version it holds and what it weighs.
pub fn facts(origin: &Origin) -> Facts {
    match origin {
        Origin::Flatpak { id, .. } => flatpak_facts(id).unwrap_or_default(),
        Origin::Snap { name } => snap_facts(name).unwrap_or_default(),
        Origin::System { manager, package } => match manager {
            Manager::Pacman => run("pacman", &["-Qi", package])
                .map(|info| parse_pacman(&info))
                .unwrap_or_default(),
            Manager::Dpkg => run(
                "dpkg-query",
                &["-W", "-f=${Version}\t${Installed-Size}", package],
            )
            .map(|fields| parse_dpkg(&fields))
            .unwrap_or_default(),
            Manager::Rpm => run(
                "rpm",
                &[
                    "-q",
                    "--queryformat",
                    "%{VERSION}-%{RELEASE}\t%{SIZE}\n",
                    package,
                ],
            )
            .map(|fields| parse_rpm(&fields))
            .unwrap_or_default(),
        },
        Origin::Unknown => Facts::default(),
    }
}

// --- who owns the entry ----------------------------------------------------

/// The application id a flatpak export belongs to.
///
/// Both installation roots — `/var/lib/flatpak` and the per-user one under
/// `~/.local/share` — export into `<root>/exports/share/applications`, and the
/// file is named after the application id. Matched on the two directory names
/// rather than on either full path, so neither root is written down here.
fn flatpak_id(path: &Path) -> Option<String> {
    let components: Vec<&str> = path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .collect();
    let root = components.iter().position(|part| *part == "flatpak")?;
    if !components[root..].contains(&"exports") {
        return None;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
}

/// Which installation that id is actually in, straight from flatpak.
///
/// Read rather than inferred from the path the entry was found under. A user
/// installation exports into the home directory and a system one into
/// `/var/lib`, but a machine can have both and the exports are merged into one
/// search path — so the directory an entry turned up in is a hint, and the
/// question being answered here decides whether removing it needs authority.
fn flatpak_scope(id: &str) -> Option<Scope> {
    let listing = run("flatpak", &["list", "--columns=application,installation"])?;
    let row = listing
        .lines()
        .map(|line| line.split('\t').map(str::trim).collect::<Vec<_>>())
        .find(|row| row.first() == Some(&id))?;
    Some(match row.get(1) {
        Some(&"user") => Scope::User,
        // Anything else is a shared installation. `system` is the usual name;
        // a machine with extra installations configured names them in
        // `installations.d`, and none of those belong to this user either.
        _ => Scope::System,
    })
}

/// The snap an entry belongs to.
///
/// Snapd exports into `/var/lib/snapd/desktop/applications`, naming each file
/// `<snap>_<app>.desktop` — one snap can ship several launchers. What can be
/// removed is the snap, so the part before the underscore is the answer.
fn snap_name(path: &Path) -> Option<String> {
    let components: Vec<&str> = path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .collect();
    if !components.contains(&"snapd") {
        return None;
    }
    let stem = path.file_stem().and_then(|stem| stem.to_str())?;
    let name = stem.split('_').next().filter(|name| !name.is_empty())?;
    Some(name.to_string())
}

fn pacman_owner(path: &Path) -> Option<String> {
    let owner = run("pacman", &["-Qoq", path.to_str()?])?;
    named(owner.lines().next()?.trim())
}

fn dpkg_owner(path: &Path) -> Option<String> {
    let owner = run("dpkg-query", &["-S", path.to_str()?])?;
    // `package: /the/path`, and a path shipped by two packages lists both.
    named(owner.lines().next()?.split(':').next()?.trim())
}

fn rpm_owner(path: &Path) -> Option<String> {
    let owner = run(
        "rpm",
        &["-qf", "--queryformat", "%{NAME}\n", path.to_str()?],
    )?;
    named(owner.lines().next()?.trim())
}

/// A package name, if it is one this shell is prepared to hand to a program.
///
/// Defence in depth rather than a real expectation. Nothing here builds a shell
/// command — every removal is an argv, so a name full of semicolons would be
/// one odd argument and not an injection — but a name that is not a name means
/// the query went wrong, and carrying that through to `pacman -R` is not worth
/// finding out about the hard way.
fn named(name: &str) -> Option<String> {
    let sane = !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.+@:/".contains(c));
    sane.then(|| name.to_string())
}

// --- what the installation says --------------------------------------------

fn flatpak_facts(id: &str) -> Option<Facts> {
    // One call rather than `flatpak info`'s labelled block: the column form is
    // tab separated and its values are not translated, so nothing here depends
    // on what the session's language is.
    let listing = run("flatpak", &["list", "--columns=application,version,size"])?;
    parse_flatpak(&listing, id)
}

fn snap_facts(name: &str) -> Option<Facts> {
    parse_snap(&run("snap", &["list", name])?)
}

/// Run a program and return what it printed, or `None` if it is not installed,
/// failed, or said nothing.
///
/// `LC_ALL=C` because several of these print labelled fields through gettext,
/// and a shell running in French would otherwise be parsing `Taille installée`.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// The two fields worth having out of `pacman -Qi`'s labelled block.
fn parse_pacman(info: &str) -> Facts {
    let field = |wanted: &str| {
        info.lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(label, _)| label.trim() == wanted)
            .map(|(_, value)| value.trim().to_string())
            .filter(|value| !value.is_empty() && value != "None")
    };
    Facts {
        version: field("Version"),
        size: field("Installed Size").as_deref().and_then(parse_size),
    }
}

/// `${Version}\t${Installed-Size}`, where dpkg's size is in kibibytes.
fn parse_dpkg(fields: &str) -> Facts {
    let (version, size) = split_pair(fields);
    Facts {
        version,
        size: size
            .and_then(|size| size.parse::<u64>().ok())
            .map(|k| k * 1024),
    }
}

/// `%{VERSION}-%{RELEASE}\t%{SIZE}`, where rpm's size is already in bytes.
fn parse_rpm(fields: &str) -> Facts {
    let (version, size) = split_pair(fields);
    Facts {
        version,
        size: size.and_then(|size| size.parse::<u64>().ok()),
    }
}

/// The row for `id` out of `flatpak list --columns=application,version,size`.
fn parse_flatpak(listing: &str, id: &str) -> Option<Facts> {
    let row = listing
        .lines()
        .map(|line| line.split('\t').map(str::trim).collect::<Vec<_>>())
        .find(|row| row.first() == Some(&id))?;
    Some(Facts {
        version: row
            .get(1)
            .map(|version| version.to_string())
            .filter(|version| !version.is_empty()),
        size: row.get(2).and_then(|size| parse_size(size)),
    })
}

/// `snap list <name>`: a header row and then the snap, in columns of spaces.
///
/// Only the version is taken. Snapd reports what a snap weighs nowhere in this
/// output, and the file under `/var/lib/snapd/snaps` is a compressed image
/// rather than the installed size, so the size row says Unknown rather than a
/// number that would be wrong in the user's favour.
fn parse_snap(listing: &str) -> Option<Facts> {
    let row = listing.lines().nth(1)?;
    let version = row.split_whitespace().nth(1)?;
    Some(Facts {
        version: Some(version.to_string()),
        size: None,
    })
}

/// The first two tab-separated fields of a one-line answer.
fn split_pair(fields: &str) -> (Option<String>, Option<String>) {
    let mut parts = fields.trim().split('\t').map(str::trim);
    let first = parts.next().filter(|field| !field.is_empty());
    let second = parts.next().filter(|field| !field.is_empty());
    (first.map(str::to_string), second.map(str::to_string))
}

/// A size a package manager has already made human — `15.73 MiB`, `396.0 MB` —
/// back into bytes.
///
/// The unit matters and is not decoration: pacman counts in kibibytes and
/// flatpak, which formats through GLib, counts in kilobytes. Reading both as
/// the same thing is a 7% error at MiB and 10% at GiB, which is exactly the
/// size of difference a user would notice between this panel and their file
/// manager.
fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit() && c != '.' && c != ',')?;
    let (number, unit) = text.split_at(split);
    // Not a locale-aware parse: `LC_ALL=C` is set on every child, so a comma
    // here is a thousands separator rather than a decimal point.
    let number: f64 = number.replace(',', "").parse().ok()?;
    let scale: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "b" | "" => 1.0,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        _ => return None,
    };
    Some((number * scale) as u64)
}

/// A size as the shell prints it.
///
/// Binary units under their own names, rather than dividing by 1024 and
/// calling the result MB the way a good deal of desktop software does. The
/// panel is one line of four, and a wrong unit on it is a thing a user can
/// catch us out on with `du`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [(&str, f64); 4] = [
        ("TiB", 1024.0 * 1024.0 * 1024.0 * 1024.0),
        ("GiB", 1024.0 * 1024.0 * 1024.0),
        ("MiB", 1024.0 * 1024.0),
        ("KiB", 1024.0),
    ];
    let bytes = bytes as f64;
    for (unit, scale) in UNITS {
        if bytes >= scale {
            let value = bytes / scale;
            // One decimal below ten, none above it: `9.4 MiB` and `247 MiB`
            // both read at a glance, where `9 MiB` throws away a tenth of the
            // answer and `247.3 MiB` is three digits nobody asked for.
            return if value < 10.0 {
                format!("{value:.1} {unit}")
            } else {
                format!("{} {unit}", value.round() as u64)
            };
        }
    }
    format!("{} B", bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture here is written by hand in the tool's shape, with invented
    /// package names: nothing installed on any particular machine belongs in
    /// this file.
    const PACMAN_INFO: &str = "\
Name            : example-editor
Version         : 3.4.1-2
Description     : An editor that does not exist
Groups          : None
Provides        : None
Installed Size  : 15.73 MiB
Build Date      : Mon 01 Jan 2035 00:00:00 GMT
";

    #[test]
    fn pacman_reports_a_version_and_a_size_in_kibibytes() {
        let facts = parse_pacman(PACMAN_INFO);
        assert_eq!(facts.version.as_deref(), Some("3.4.1-2"));
        assert_eq!(facts.size, Some((15.73 * 1024.0 * 1024.0) as u64));
    }

    /// A field pacman prints as `None` is not a value, and neither is a label
    /// this does not recognise appearing where the version should be.
    #[test]
    fn pacman_fields_that_say_nothing_are_not_answers() {
        let facts = parse_pacman("Name            : example-editor\nProvides        : None\n");
        assert_eq!(facts, Facts::default());
    }

    #[test]
    fn dpkg_counts_in_kibibytes_and_rpm_in_bytes() {
        let dpkg = parse_dpkg("2:9.1.0016-1\t3200\n");
        assert_eq!(dpkg.version.as_deref(), Some("2:9.1.0016-1"));
        assert_eq!(dpkg.size, Some(3200 * 1024));

        let rpm = parse_rpm("3.4.1-2.fc41\t16494981\n");
        assert_eq!(rpm.version.as_deref(), Some("3.4.1-2.fc41"));
        assert_eq!(rpm.size, Some(16_494_981));
    }

    /// The flatpak listing holds every application on the machine; the one
    /// asked about is found by id and nothing else is read.
    #[test]
    fn the_flatpak_listing_is_searched_by_application_id() {
        let listing = "\
com.example.First\t1.2.3\t10.8 MB
com.example.Second\t4.5\t396.0 MB
";
        let facts = parse_flatpak(listing, "com.example.Second").expect("the row is there");
        assert_eq!(facts.version.as_deref(), Some("4.5"));
        assert_eq!(facts.size, Some(396_000_000));
        assert_eq!(parse_flatpak(listing, "com.example.Missing"), None);
    }

    /// Snapd reports no size, and saying so is the answer rather than working
    /// one out from the compressed image on disk.
    #[test]
    fn a_snap_reports_its_version_and_no_size() {
        let listing = "\
Name           Version   Rev    Tracking       Publisher   Notes
example-note   2.9.1     412    latest/stable  someone     -
";
        let facts = parse_snap(listing).expect("the row is there");
        assert_eq!(facts.version.as_deref(), Some("2.9.1"));
        assert_eq!(facts.size, None);
        assert_eq!(parse_snap("Name  Version\n"), None);
    }

    /// The half of this that is easy to get wrong: MB and MiB are different
    /// numbers, and both turn up in the answers the shell parses.
    #[test]
    fn binary_and_decimal_units_are_told_apart() {
        assert_eq!(parse_size("1 MiB"), Some(1024 * 1024));
        assert_eq!(parse_size("1 MB"), Some(1_000_000));
        assert_eq!(parse_size("8.00 KiB"), Some(8192));
        assert_eq!(parse_size("512 B"), Some(512));
        assert_eq!(parse_size("1,024 KiB"), Some(1024 * 1024));
        assert_eq!(parse_size("wafers"), None);
        assert_eq!(parse_size("12 furlongs"), None);
    }

    #[test]
    fn sizes_are_printed_with_one_decimal_only_where_it_says_anything() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(8192), "8.0 KiB");
        assert_eq!(human_size(9_857_433), "9.4 MiB");
        assert_eq!(human_size(259_000_000), "247 MiB");
        assert_eq!(human_size(3_221_225_472), "3.0 GiB");
    }

    /// A flatpak export is recognised from the two directory names it always
    /// sits under, so both installation roots work and neither is written down.
    #[test]
    fn a_flatpak_export_is_recognised_by_its_directories() {
        let system =
            Path::new("/var/lib/flatpak/exports/share/applications/com.example.Thing.desktop");
        assert_eq!(flatpak_id(system).as_deref(), Some("com.example.Thing"));

        let user = Path::new(
            "/home/someone/.local/share/flatpak/exports/share/applications/com.example.Thing.desktop",
        );
        assert_eq!(flatpak_id(user).as_deref(), Some("com.example.Thing"));

        // An ordinary system entry is nobody's export, even one belonging to
        // the flatpak tool itself.
        assert_eq!(
            flatpak_id(Path::new("/usr/share/applications/flatpak.desktop")),
            None
        );
    }

    /// A snap ships one entry per launcher, all named after the snap; what can
    /// be removed is the snap, so that is what is read out.
    #[test]
    fn a_snap_is_recognised_by_the_name_in_front_of_the_underscore() {
        let path = Path::new("/var/lib/snapd/desktop/applications/example-note_editor.desktop");
        assert_eq!(snap_name(path).as_deref(), Some("example-note"));

        // A snap whose launcher is named after the snap itself.
        let plain =
            Path::new("/var/lib/snapd/desktop/applications/example-note_example-note.desktop");
        assert_eq!(snap_name(plain).as_deref(), Some("example-note"));

        assert_eq!(
            snap_name(Path::new("/usr/share/applications/thing_other.desktop")),
            None
        );
    }

    /// The package name is what the removal command is handed, so a query that
    /// went wrong must not become an argument.
    #[test]
    fn a_package_name_that_is_not_one_is_refused() {
        assert_eq!(
            named("nm-connection-editor").as_deref(),
            Some("nm-connection-editor")
        );
        assert_eq!(named("lib32-mesa").as_deref(), Some("lib32-mesa"));
        assert_eq!(named("").as_deref(), None);
        assert_eq!(named("error: no package owns").as_deref(), None);
        assert_eq!(named("--assume-installed").as_deref(), None);
        assert_eq!(named("thing; rm -rf /").as_deref(), None);
    }

    /// A machine where nothing owns the entry answers "I do not know", which is
    /// the answer the shell is built to act on.
    #[test]
    fn an_unowned_entry_is_unknown_rather_than_a_guess() {
        let origin = identify(Path::new("/nonexistent/lxb-test/never-installed.desktop"));
        assert_eq!(origin, Origin::Unknown);
        assert_eq!(origin.package(), None);
        assert_eq!(facts(&origin), Facts::default());
    }
}
