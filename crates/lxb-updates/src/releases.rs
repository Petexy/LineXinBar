//! LineXinBar and the six projects released beside it, kept up to date from
//! their own releases on GitHub wherever the system's repositories do not
//! carry them.
//!
//! A distribution that packages this desktop updates it with everything else,
//! and then nothing here has anything to do: the source does not even appear.
//! But at the start almost none will, and a desktop somebody installed from a
//! release page would then stay at that release for ever — the system's
//! update does not know it exists. So every project in [`FAMILY`] publishes
//! its versions as tagged releases, with the Arch, Debian and Fedora packages
//! attached, and this is the source that reads them.
//!
//! Which copies are this source's to update, and which are not, is the whole
//! of the policy, and it is decided per project:
//!
//! - **Installed as packages none of the system's repositories offer** — a
//!   `.pkg.tar.zst`, `.deb` or `.rpm` somebody downloaded from a release. The
//!   newer release's packages for the same format are downloaded, checked
//!   against the SHA-256 GitHub publishes for each file, and handed to the
//!   system's own package manager in one transaction. A project is taken only
//!   when *every* package of it installed is foreign: the shell's packages are
//!   version-locked to one another, and half of them from a repository and
//!   half from a release is a dependency the package manager will refuse.
//! - **Installed from source**, with the project's own `packaging/install.sh`,
//!   under `/usr`, `/usr/local` or `~/.local`, owned by no package. This is
//!   Gentoo and every distribution no release carries a package for: the tag
//!   is cloned, built with `cargo build --release --locked` and installed with
//!   the tag's own `install.sh`, exactly as the READMEs say to by hand.
//! - **Anything a repository carries** is the system's, and left to it.
//!
//! The versions come from the releases' tags (`v0.9.0-alpha`), pre-releases
//! included — the first release of all seven is one. A release is offered
//! when it is newer than what is installed, never otherwise: a machine running
//! a development build newer than anything released is up to date, not a
//! machine to be downgraded.
use crate::{discovery::Host, process, Item, System};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Whose GitHub account the family is released under.
pub const OWNER: &str = "Petexy";
pub(crate) const API: &str = "https://api.github.com";

/// Reads the release lists from `<dir>/<repo>.json` instead of from GitHub.
/// A test's; honoured only by the check, which decides nothing on its own —
/// whatever it lists, the step that installs asks GitHub itself.
const FIXTURES: &str = "LXB_UPDATES_RELEASE_FIXTURES";

/// One project of the family: where it is released, what its packages are
/// called, and how a copy built from its source is recognised.
pub struct Project {
    /// The repository under [`OWNER`], which is also how the project is
    /// written: `LineXinBar`, `CEDM`, `lxb-toolkit`, …
    pub repo: &'static str,
    /// Every package any of its releases attaches, in any format.
    pub packages: &'static [&'static str],
    /// The parts `packaging/install.sh` stages, each with the file that says
    /// it is installed.
    pub parts: &'static [Part],
    /// Executables under the prefix that answer `--version`, the first one
    /// present being asked. Empty for the toolkit, whose version is in its
    /// pkg-config file.
    pub reports: &'static [&'static str],
}

pub struct Part {
    /// What `install.sh --component` calls it; empty for a project whose
    /// script takes no `--component`.
    pub component: &'static str,
    pub marker: Marker,
}

pub enum Marker {
    /// A path under the prefix.
    Prefix(&'static str),
    /// A file in the library directory, which is `lib`, `lib64` or a
    /// multiarch directory under the prefix, as the distribution wants.
    Library(&'static str),
    /// A package directory in python's site directory under the prefix.
    Python(&'static str),
}

/// The family, in the order it has to be installed in: the shell before the
/// greeter that runs its compositor, the toolkit before the applications
/// whose builds compile its sources in.
pub const FAMILY: &[Project] = &[
    Project {
        repo: "LineXinBar",
        packages: &[
            "lxb-compositor",
            "lxb-desktop",
            "lxb-retroarch",
            "lxb-heroic",
        ],
        parts: &[
            Part {
                component: "compositor",
                marker: Marker::Prefix("bin/lxb"),
            },
            Part {
                component: "desktop",
                marker: Marker::Prefix("bin/lxb-desktop"),
            },
            Part {
                component: "retroarch",
                marker: Marker::Prefix("bin/lxb-retroarch"),
            },
            Part {
                component: "heroic",
                marker: Marker::Prefix("bin/lxb-heroic"),
            },
        ],
        reports: &["bin/lxb-updates", "bin/lxb"],
    },
    Project {
        repo: "lxb-toolkit",
        packages: &[
            "lxb-toolkit",
            "lxb-toolkit-dev",
            "lxb-toolkit-devel",
            "python-lxb-toolkit",
            "python3-lxb-toolkit",
        ],
        parts: &[
            Part {
                component: "library",
                marker: Marker::Library("liblxb_toolkit.so"),
            },
            Part {
                component: "devel",
                marker: Marker::Prefix("share/lxb-toolkit/crates"),
            },
            Part {
                component: "python",
                marker: Marker::Python("lxb_toolkit"),
            },
        ],
        reports: &[],
    },
    Project {
        repo: "CEDM",
        packages: &["cedm"],
        parts: &[Part {
            component: "",
            marker: Marker::Prefix("bin/cedm"),
        }],
        reports: &["bin/cedm"],
    },
    Project {
        repo: "DistriBumpy",
        packages: &["distribumpy"],
        parts: &[Part {
            component: "",
            marker: Marker::Prefix("bin/distribumpy"),
        }],
        reports: &["bin/distribumpy"],
    },
    Project {
        repo: "ImagOnSole",
        packages: &["imagonsole"],
        parts: &[Part {
            component: "",
            marker: Marker::Prefix("bin/imagonsole"),
        }],
        reports: &["bin/imagonsole"],
    },
    Project {
        repo: "VideOnSole",
        packages: &["videonsole"],
        parts: &[Part {
            component: "",
            marker: Marker::Prefix("bin/videonsole"),
        }],
        reports: &["bin/videonsole"],
    },
    Project {
        repo: "SongOnSole",
        packages: &["songonsole"],
        parts: &[Part {
            component: "",
            marker: Marker::Prefix("bin/songonsole"),
        }],
        reports: &["bin/songonsole"],
    },
];

pub fn project(repo: &str) -> Option<&'static Project> {
    FAMILY.iter().find(|p| p.repo == repo)
}

/// Whether an item of this source is the desktop itself or the login screen
/// in front of it — a package's name, or a project's where it is built from
/// source. Those run for as long as the session does, so what was installed
/// is in use only from the next sign-in; an application is new the next time
/// it is opened.
pub fn restarts_the_session(item: &str) -> bool {
    matches!(
        item,
        "lxb-compositor" | "lxb-desktop" | "LineXinBar" | "cedm" | "CEDM"
    )
}

/// Where a copy built from source may be installed as root: the two prefixes
/// the READMEs install to.
pub const SYSTEM_PREFIXES: [&str; 2] = ["/usr", "/usr/local"];

// ───────────────────────────── versions ─────────────────────────────

/// A release's version, read off its tag: `v0.9.0-alpha` is 0.9.0 with the
/// pre-release `alpha`, which comes before 0.9.0 itself. Semantic-versioning
/// order, without insisting on exactly three numbers — so `0.9` and `0.9.0`
/// are the same version, and equal as well as ordered alike.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub numbers: Vec<u64>,
    pub pre: Vec<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text
            .strip_prefix('v')
            .or_else(|| text.strip_prefix('V'))
            .unwrap_or(text);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((_, "")) => return None,
            Some((core, pre)) => (core, pre),
            None => (text, ""),
        };
        let numbers: Vec<u64> = core
            .split('.')
            .map(|n| {
                (!n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| n.parse().ok())
                    .flatten()
            })
            .collect::<Option<_>>()?;
        if numbers.is_empty() || numbers.len() > 4 {
            return None;
        }
        let pre: Vec<String> = if pre.is_empty() {
            vec![]
        } else {
            pre.split('.').map(str::to_owned).collect()
        };
        if pre.iter().any(|p| {
            p.is_empty()
                || p.len() > 32
                || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) {
            return None;
        }
        Some(Self { numbers, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let length = self.numbers.len().max(other.numbers.len());
        for index in 0..length {
            let a = self.numbers.get(index).copied().unwrap_or(0);
            let b = other.numbers.get(index).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                order => return order,
            }
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            _ => {}
        }
        for (a, b) in self.pre.iter().zip(&other.pre) {
            let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                _ => a.cmp(b),
            };
            if order != Ordering::Equal {
                return order;
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Version {}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let numbers: Vec<String> = self.numbers.iter().map(u64::to_string).collect();
        write!(f, "{}", numbers.join("."))?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// Order two package versions as the package managers do —
/// `[epoch:]version[-release]`, compared the way pacman's `vercmp` and RPM
/// compare them, `~` sorting before everything. Debian's rules differ in
/// corners no version of this family comes near.
pub fn vercmp(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> (u64, &str, Option<&str>) {
        let (epoch, rest) = match v.split_once(':') {
            Some((e, rest)) if e.bytes().all(|b| b.is_ascii_digit()) && !e.is_empty() => {
                (e.parse().unwrap_or(0), rest)
            }
            _ => (0, v),
        };
        match rest.rsplit_once('-') {
            Some((version, release)) => (epoch, version, Some(release)),
            None => (epoch, rest, None),
        }
    }
    let (ea, va, ra) = split(a);
    let (eb, vb, rb) = split(b);
    ea.cmp(&eb)
        .then_with(|| segments(va, vb))
        .then_with(|| match (ra, rb) {
            (Some(ra), Some(rb)) => segments(ra, rb),
            _ => Ordering::Equal,
        })
}

/// RPM's `rpmvercmp`: runs of digits against runs of digits by value, runs of
/// letters against letters as text, a digit run newer than a letter run.
fn segments(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        let skip = |s: &[u8]| {
            s.iter()
                .position(|c| c.is_ascii_alphanumeric() || *c == b'~')
                .unwrap_or(s.len())
        };
        a = &a[skip(a)..];
        b = &b[skip(b)..];
        match (a.first(), b.first()) {
            (Some(b'~'), Some(b'~')) => {
                a = &a[1..];
                b = &b[1..];
                continue;
            }
            (Some(b'~'), _) => return Ordering::Less,
            (_, Some(b'~')) => return Ordering::Greater,
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            _ => {}
        }
        let numeric = a[0].is_ascii_digit();
        let run = |s: &[u8]| {
            s.iter()
                .position(|c| {
                    if numeric {
                        !c.is_ascii_digit()
                    } else {
                        !c.is_ascii_alphabetic()
                    }
                })
                .unwrap_or(s.len())
        };
        let (la, lb) = (run(a), run(b));
        if lb == 0 {
            // A digit run against a letter run: the number is newer.
            return if numeric {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        let (sa, sb) = (&a[..la], &b[..lb]);
        let order = if numeric {
            let trim = |s: &[u8]| {
                let zeros = s.iter().take_while(|c| **c == b'0').count();
                s[zeros..].to_vec()
            };
            let (ta, tb) = (trim(sa), trim(sb));
            ta.len().cmp(&tb.len()).then_with(|| ta.cmp(&tb))
        } else {
            sa.cmp(sb)
        };
        if order != Ordering::Equal {
            return order;
        }
        a = &a[la..];
        b = &b[lb..];
    }
}

/// A version out of what a program says to `--version` or what a
/// pkg-config file says after `Version:` — the last word that reads as one.
pub fn reported(text: &str) -> Option<Version> {
    text.split_whitespace().rev().find_map(Version::parse)
}

// ───────────────────────────── the host ─────────────────────────────

/// The package format this machine installs, if a release carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Pacman,
    Deb,
    /// Fedora and what is built on it, at this Fedora release.
    Rpm {
        fedora: u32,
        dnf5: bool,
    },
}

impl Format {
    pub fn of(host: &Host, system: System) -> Option<Self> {
        match system {
            System::Pacman => Some(Self::Pacman),
            System::Apt => Some(Self::Deb),
            System::Dnf5 | System::Dnf => {
                let fedora = host.release.get("ID").is_some_and(|id| id == "fedora")
                    || host
                        .release
                        .get("ID_LIKE")
                        .is_some_and(|like| like.split_whitespace().any(|id| id == "fedora"));
                let version = host.release.get("VERSION_ID")?.parse().ok()?;
                fedora.then_some(Self::Rpm {
                    fedora: version,
                    dnf5: system == System::Dnf5,
                })
            }
            _ => None,
        }
    }

    /// The architecture a release names its packages for on this machine.
    pub fn arch(self) -> String {
        let machine = machine();
        match self {
            Self::Deb => match machine.as_str() {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                "armv7l" => "armhf",
                "i386" | "i586" | "i686" => "i386",
                "ppc64le" => "ppc64el",
                other => other,
            }
            .into(),
            _ => machine,
        }
    }

    /// Every family package installed through the package manager, with its
    /// version.
    fn installed(self) -> Result<BTreeMap<String, String>> {
        let wanted: BTreeSet<&str> = FAMILY.iter().flat_map(|p| p.packages).copied().collect();
        let mut found = BTreeMap::new();
        match self {
            Self::Pacman => {
                for line in process::probe("pacman", &["-Q"], &[0])?.text.lines() {
                    if let Some((name, version)) = line.split_once(' ') {
                        if wanted.contains(name) {
                            found.insert(name.to_owned(), version.trim().to_owned());
                        }
                    }
                }
            }
            Self::Deb => {
                let listing = process::probe(
                    "dpkg-query",
                    &["-W", "-f", "${db:Status-Abbrev} ${Package} ${Version}\\n"],
                    &[0],
                )?;
                for line in listing.text.lines() {
                    let words: Vec<&str> = line.split_whitespace().collect();
                    if let [status, name, version] = words[..] {
                        if status.starts_with("ii") && wanted.contains(name) {
                            found.insert(name.to_owned(), version.to_owned());
                        }
                    }
                }
            }
            Self::Rpm { .. } => {
                let listing = process::probe(
                    "rpm",
                    &["-qa", "--qf", "%{NAME} %{EPOCH}:%{VERSION}-%{RELEASE}\\n"],
                    &[0],
                )?;
                for line in listing.text.lines() {
                    if let Some((name, version)) = line.split_once(' ') {
                        if wanted.contains(name) {
                            let version = version.trim().trim_start_matches("(none):");
                            found.insert(name.to_owned(), version.to_owned());
                        }
                    }
                }
            }
        }
        Ok(found)
    }

    /// Which of `names` no repository of this system offers.
    ///
    /// Asked of the package manager's local lists only, as the rest of a
    /// discovery is: pacman's sync databases, APT's package lists, DNF's
    /// metadata cache. A DNF with no cache yet cannot say, and then the
    /// packages are taken to be foreign — the step that installs still
    /// refuses anything that is not newer than what is there.
    fn foreign(self, names: &[&str]) -> Result<BTreeSet<String>> {
        if names.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut foreign = BTreeSet::new();
        match self {
            Self::Pacman => {
                let listing = process::ask("pacman", &["-Qqm"])?;
                if listing.code > 1 {
                    bail!("pacman -Qqm exited {}", listing.code);
                }
                for line in listing.text.lines() {
                    if names.contains(&line.trim()) {
                        foreign.insert(line.trim().to_owned());
                    }
                }
            }
            Self::Deb => {
                let mut args = vec!["policy"];
                args.extend(names);
                let policy = process::probe("apt-cache", &args, &[0])?;
                for name in names {
                    if !apt_carries(&policy.text, name) {
                        foreign.insert((*name).to_owned());
                    }
                }
            }
            Self::Rpm { dnf5, .. } => {
                let dnf = if dnf5 { "dnf5" } else { "dnf" };
                let mut args = vec![
                    "repoquery",
                    "--available",
                    "--cacheonly",
                    "--queryformat",
                    "%{name}\\n",
                ];
                args.extend(names);
                let offered: BTreeSet<String> = match process::probe(dnf, &args, &[0]) {
                    Ok(output) => output
                        .text
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    Err(_) => BTreeSet::new(),
                };
                for name in names {
                    if !offered.contains(*name) {
                        foreign.insert((*name).to_owned());
                    }
                }
            }
        }
        Ok(foreign)
    }
}

/// Whether `apt-cache policy` names a source for `package` other than the
/// status file — which is where a `.deb` installed by hand is the only
/// version there is.
pub fn apt_carries(policy: &str, package: &str) -> bool {
    let mut inside = false;
    for line in policy.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            inside = line.trim_end_matches(':') == package;
            continue;
        }
        if !inside {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() >= 2
            && words[0].parse::<i32>().is_ok()
            && (words[1].starts_with('/') || words[1].contains("://"))
            && words[1] != "/var/lib/dpkg/status"
        {
            return true;
        }
    }
    false
}

fn machine() -> String {
    let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();
    if unsafe { libc::uname(name.as_mut_ptr()) } != 0 {
        return String::new();
    }
    let name = unsafe { name.assume_init() };
    unsafe { std::ffi::CStr::from_ptr(name.machine.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

// ─────────────────────────── what is installed ───────────────────────────

/// A family project this source updates, as it is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Packages no repository offers, with their installed versions.
    Packaged {
        repo: &'static str,
        packages: Vec<(String, String)>,
    },
    /// Built from source and installed under `prefix`.
    Built(Built),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub repo: &'static str,
    pub prefix: PathBuf,
    pub components: Vec<&'static str>,
    pub libdir: Option<PathBuf>,
    pub sitedir: Option<PathBuf>,
}

/// What this source would update on this machine: nothing, on a machine
/// whose repositories carry the family.
///
/// Local only — package databases and the file system, never the network —
/// because this decides whether the source is listed at all, and that is
/// asked on every check.
pub fn find(host: &Host) -> Vec<Found> {
    let system = host.system();
    let format = Format::of(host, system);
    let installed = format.and_then(|f| f.installed().ok()).unwrap_or_default();
    let mut found = vec![];
    for project in FAMILY {
        let packages: Vec<(String, String)> = project
            .packages
            .iter()
            .filter_map(|name| {
                installed
                    .get(*name)
                    .map(|v| ((*name).to_owned(), v.clone()))
            })
            .collect();
        if !packages.is_empty() {
            let names: Vec<&str> = packages.iter().map(|(n, _)| n.as_str()).collect();
            let foreign = format
                .and_then(|f| f.foreign(&names).ok())
                .unwrap_or_default();
            if names.iter().all(|n| foreign.contains(*n)) {
                found.push(Found::Packaged {
                    repo: project.repo,
                    packages,
                });
            }
            // Installed as a package, so a copy of the same project built
            // from source beside it would be one this machine has two of,
            // which is nothing for an updater to guess about.
            continue;
        }
        for prefix in prefixes() {
            if let Some(built) = built(project, &prefix, system) {
                found.push(Found::Built(built));
            }
        }
    }
    found
}

fn prefixes() -> Vec<PathBuf> {
    let mut prefixes: Vec<PathBuf> = SYSTEM_PREFIXES.iter().map(PathBuf::from).collect();
    if let Some(home) = home() {
        prefixes.push(home.join(".local"));
    }
    prefixes
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p != Path::new("/"))
}

/// The library directories a distribution may have put a library under.
fn libdirs(prefix: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![prefix.join("lib"), prefix.join("lib64")];
    if let Ok(entries) = std::fs::read_dir(prefix.join("lib")) {
        let mut multiarch: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("-linux-gnu") || n.ends_with("-linux-musl"))
            })
            .collect();
        multiarch.sort();
        dirs.extend(multiarch);
    }
    dirs
}

fn sitedirs(prefix: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![];
    for lib in ["lib", "lib64"] {
        if let Ok(entries) = std::fs::read_dir(prefix.join(lib)) {
            let mut pythons: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("python3"))
                })
                .collect();
            pythons.sort();
            for python in pythons {
                dirs.push(python.join("site-packages"));
                dirs.push(python.join("dist-packages"));
            }
        }
    }
    dirs
}

/// A copy of `project` built from source under `prefix`, if there is one no
/// package owns.
pub fn built(project: &'static Project, prefix: &Path, system: System) -> Option<Built> {
    let mut components = vec![];
    let mut libdir = None;
    let mut sitedir = None;
    let mut markers = vec![];
    for part in project.parts {
        let path = match part.marker {
            Marker::Prefix(path) => Some(prefix.join(path)),
            Marker::Library(name) => libdirs(prefix)
                .into_iter()
                .find(|dir| dir.join(name).exists())
                .map(|dir| {
                    let path = dir.join(name);
                    libdir = Some(dir);
                    path
                }),
            // Only beside the rest of the project: python's own directory
            // is not somewhere a copy of the toolkit is recognised by alone.
            Marker::Python(name) if !components.is_empty() => sitedirs(prefix)
                .into_iter()
                .find(|dir| dir.join(name).is_dir())
                .map(|dir| {
                    let path = dir.join(name);
                    sitedir = Some(dir);
                    path
                }),
            Marker::Python(_) => None,
        };
        if let Some(path) = path.filter(|p| p.symlink_metadata().is_ok()) {
            components.push(part.component);
            markers.push(path);
        }
    }
    if components.is_empty() {
        return None;
    }
    // Nothing a package manager installed is this source's. Under /usr that
    // has to be asked of whichever one this is; a system that cannot be
    // asked gets no update there at all rather than a guess. /usr/local and
    // a home directory are nobody's package by convention.
    if prefix == Path::new("/usr") && markers.iter().any(|m| owned(system, m) != Some(false)) {
        return None;
    }
    Some(Built {
        repo: project.repo,
        prefix: prefix.to_owned(),
        components,
        libdir,
        sitedir,
    })
}

/// Whether the system's package manager owns `path`: `None` where this
/// system cannot be asked.
fn owned(system: System, path: &Path) -> Option<bool> {
    let path_text = path.to_str()?;
    let answer = |program: &str, args: &[&str]| -> Option<bool> {
        match process::ask(program, args).ok()?.code {
            0 => Some(true),
            1 => Some(false),
            _ => None,
        }
    };
    use System as S;
    match system {
        S::Pacman => answer("pacman", &["-Qqo", path_text]),
        S::Apt => answer("dpkg-query", &["-S", path_text]),
        S::Dnf5 | S::Dnf | S::ZypperLeap | S::ZypperRolling | S::Urpmi => {
            answer("rpm", &["-qf", path_text])
        }
        S::Portage => portage_owns(Path::new("/var/db/pkg"), path_text),
        _ => None,
    }
}

/// Portage keeps what each package installed in its `CONTENTS`, one line a
/// path: `obj /usr/bin/x <md5> <mtime>`, `sym /usr/bin/y -> x <mtime>`.
fn portage_owns(database: &Path, path: &str) -> Option<bool> {
    let categories = std::fs::read_dir(database).ok()?;
    for category in categories.flatten() {
        let Ok(packages) = std::fs::read_dir(category.path()) else {
            continue;
        };
        for package in packages.flatten() {
            let Ok(contents) = std::fs::read_to_string(package.path().join("CONTENTS")) else {
                continue;
            };
            if contents.lines().any(|line| {
                let mut words = line.splitn(3, ' ');
                matches!(words.next(), Some("obj" | "sym" | "dir")) && words.next() == Some(path)
            }) {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// What the updater itself last installed of a project under a prefix: the
/// tag, and every file, so that the next update can tell `0.9.0-alpha` from
/// `0.9.0` (a binary says 0.9.0 to both) and take away what a newer release
/// no longer ships.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub tag: String,
    pub files: Vec<PathBuf>,
}

pub fn record_path(prefix: &Path, repo: &str) -> PathBuf {
    prefix.join(format!("share/lxb-updates/installed/{repo}.json"))
}

pub fn record(prefix: &Path, repo: &str) -> Option<Record> {
    let text = std::fs::read_to_string(record_path(prefix, repo)).ok()?;
    serde_json::from_str(&text).ok()
}

/// What version a copy built from source is at.
pub fn built_version(built: &Built) -> Option<Version> {
    if let Some(record) = record(&built.prefix, built.repo) {
        if let Some(version) = Version::parse(&record.tag) {
            return Some(version);
        }
    }
    let project = project(built.repo)?;
    if project.reports.is_empty() {
        let libdirs = built
            .libdir
            .iter()
            .cloned()
            .chain(libdirs(&built.prefix))
            .collect::<Vec<_>>();
        return libdirs.iter().find_map(|dir| {
            let text =
                std::fs::read_to_string(dir.join(format!("pkgconfig/{}.pc", project.repo))).ok()?;
            text.lines()
                .find_map(|l| l.strip_prefix("Version:"))
                .and_then(reported)
        });
    }
    let program = project
        .reports
        .iter()
        .map(|r| built.prefix.join(r))
        .find(|p| p.is_file())?;
    // Asked by running it, which as root is a program run as root: only one
    // nobody but root could have put there, as every root step's program is.
    let program = if unsafe { libc::geteuid() } == 0 {
        process::trusted(program).ok()?
    } else {
        program
    };
    let mut command = std::process::Command::new(&program);
    command
        .arg("--version")
        .env("LC_ALL", "C")
        .env("NO_COLOR", "1")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY");
    let output = process::ask_command(command, &program.to_string_lossy()).ok()?;
    (output.code == 0).then(|| reported(&output.text)).flatten()
}

// ───────────────────────────── releases ─────────────────────────────

/// A release as GitHub's API describes one, cut down to what is read.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    /// `sha256:<hex>`, which GitHub computes when the file is uploaded.
    #[serde(default)]
    pub digest: Option<String>,
    pub browser_download_url: String,
}

/// The newest published release: the highest version among the ones that
/// are not drafts and whose tags read as versions.
pub fn newest(releases: &[Release]) -> Option<(&Release, Version)> {
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| Some((r, Version::parse(&r.tag_name)?)))
        .max_by(|a, b| a.1.cmp(&b.1))
}

/// One package file of a release, as its name describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub file: String,
    pub name: String,
    /// `version-release`, the package's own version.
    pub version: String,
    pub url: String,
    pub size: u64,
    pub digest: Option<String>,
}

/// The file of `release` that installs `package` on this format and
/// architecture, if it attaches one.
///
/// Fedora's files carry the Fedora they were built for (`1.fc44`). The one
/// for this Fedora is taken, or else the newest built for an older one —
/// packages built for an older release install on a newer one far more often
/// than the other way round, and the package manager refuses one whose
/// libraries are not there.
pub fn package_for(format: Format, arch: &str, assets: &[Asset], package: &str) -> Option<Package> {
    let mut best: Option<(u32, Package)> = None;
    for asset in assets {
        let Some((name, version, rank)) = parse_package(format, arch, &asset.name) else {
            continue;
        };
        if name != package {
            continue;
        }
        let found = Package {
            file: asset.name.clone(),
            name,
            version,
            url: asset.browser_download_url.clone(),
            size: asset.size,
            digest: asset.digest.clone(),
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, found));
        }
    }
    best.map(|(_, package)| package)
}

/// A file name's package, version and how well it fits this machine, or
/// `None` where it is not a package for this machine.
fn parse_package(format: Format, arch: &str, file: &str) -> Option<(String, String, u32)> {
    match format {
        Format::Pacman => {
            let stem = file
                .strip_suffix(".pkg.tar.zst")
                .or_else(|| file.strip_suffix(".pkg.tar.xz"))?;
            let mut parts = stem.rsplitn(4, '-');
            let (file_arch, release, version, name) =
                (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            (file_arch == arch || file_arch == "any")
                .then(|| (name.to_owned(), format!("{version}-{release}"), 1))
        }
        Format::Deb => {
            let stem = file.strip_suffix(".deb")?;
            let mut parts = stem.splitn(3, '_');
            let (name, version, file_arch) = (parts.next()?, parts.next()?, parts.next()?);
            (file_arch == arch || file_arch == "all")
                .then(|| (name.to_owned(), version.to_owned(), 1))
        }
        Format::Rpm { fedora, .. } => {
            let stem = file.strip_suffix(".rpm")?;
            let (stem, file_arch) = stem.rsplit_once('.')?;
            if file_arch != arch && file_arch != "noarch" {
                return None;
            }
            let mut parts = stem.rsplitn(3, '-');
            let (release, version, name) = (parts.next()?, parts.next()?, parts.next()?);
            let built_for = release
                .split('.')
                .find_map(|part| part.strip_prefix("fc")?.parse::<u32>().ok())?;
            (built_for <= fedora)
                .then(|| (name.to_owned(), format!("{version}-{release}"), built_for))
        }
    }
}

/// The release list of `repo`, newest first as GitHub orders it.
///
/// Asked with the last answer's `ETag`, so that a check that finds nothing
/// new is answered `304 Not Modified` from the copy kept beside the journal.
/// The API allows sixty unauthenticated requests an hour from an address, and
/// seven projects asked on every press of Check again would spend them.
pub fn releases(repo: &str) -> Result<Vec<Release>> {
    if let Some(dir) = std::env::var_os(FIXTURES) {
        let text = std::fs::read_to_string(Path::new(&dir).join(format!("{repo}.json")))
            .with_context(|| format!("No release fixture for {repo}"))?;
        return Ok(serde_json::from_str(&text)?);
    }
    #[derive(Serialize, Deserialize)]
    struct Kept {
        etag: String,
        body: String,
    }
    let kept_path = crate::service::directory()
        .ok()
        .map(|d| d.join("releases").join(format!("{repo}.json")));
    let kept: Option<Kept> = kept_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok());
    let url = format!("{API}/repos/{OWNER}/{repo}/releases?per_page=20");
    let mut request = agent().get(&url);
    if let Some(kept) = &kept {
        request = request.header("If-None-Match", &kept.etag);
    }
    let mut response = request
        .call()
        .map_err(|error| anyhow::anyhow!("GitHub could not be reached: {error}"))?;
    let status = response.status().as_u16();
    let body = match status {
        304 => kept
            .map(|k| k.body)
            .context("GitHub answered with nothing to reuse")?,
        200 => {
            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body = response
                .body_mut()
                .with_config()
                .limit(8 * 1024 * 1024)
                .read_to_string()
                .context("GitHub's answer could not be read")?;
            if let (Some(etag), Some(path)) = (etag, &kept_path) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(
                    path,
                    serde_json::to_vec(&Kept {
                        etag,
                        body: body.clone(),
                    })?,
                );
            }
            body
        }
        _ => bail!("{}", refused(repo, status, &response)),
    };
    serde_json::from_str(&body)
        .with_context(|| format!("{repo}'s release list is not what GitHub sends"))
}

/// The release tagged `tag`, asked of GitHub directly. What the step that
/// installs reads: it decides from this, never from what the check said.
pub fn release(repo: &str, tag: &str) -> Result<Release> {
    let url = format!("{API}/repos/{OWNER}/{repo}/releases/tags/{tag}");
    let mut response = agent()
        .get(&url)
        .call()
        .map_err(|error| anyhow::anyhow!("GitHub could not be reached: {error}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        bail!("{}", refused(repo, status, &response));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()?;
    let release: Release = serde_json::from_str(&body)?;
    if release.draft || release.tag_name != tag {
        bail!("{repo} has no published release tagged {tag}");
    }
    Ok(release)
}

fn refused(repo: &str, status: u16, response: &ureq::http::Response<ureq::Body>) -> String {
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    if (status == 403 || status == 429) && header("x-ratelimit-remaining").as_deref() == Some("0") {
        let minutes = header("x-ratelimit-reset")
            .and_then(|t| t.parse::<u64>().ok())
            .map(|reset| reset.saturating_sub(crate::now()).div_ceil(60))
            .unwrap_or(60);
        return format!(
            "GitHub is not answering more checks from this address for about {minutes} minutes"
        );
    }
    if status == 404 {
        return format!("GitHub has no releases for {OWNER}/{repo}");
    }
    format!("GitHub answered {status} for {repo}'s releases")
}

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .user_agent(concat!("lxb-updates/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

// ────────────────────────────── the plan ──────────────────────────────

/// Something the review authorizes: what the check decided, carried to the
/// step that does it and checked again there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    /// Every package file named, from the releases named, in one transaction
    /// of the system's package manager.
    Packages { targets: Vec<Target> },
    /// One project's tag, built and installed under `prefix`.
    Source {
        repo: String,
        tag: String,
        prefix: String,
        components: Vec<String>,
        #[serde(default)]
        libdir: Option<String>,
        #[serde(default)]
        sitedir: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub repo: String,
    pub tag: String,
    pub packages: Vec<String>,
}

pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && !tag.starts_with('-')
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        && Version::parse(tag).is_some()
}

/// A path with nothing in it but names under the root, so that "under the
/// prefix" means what it says: `/usr/../etc` starts with `/usr` to
/// [`Path::starts_with`].
pub fn plain_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// A path written the one way it can be: absolute, with no `.` or `..` and
/// no doubled or trailing separator, so that "under the prefix" means what
/// it says.
fn plain(path: &str) -> bool {
    let p = Path::new(path);
    p.is_absolute()
        && path.len() <= 512
        && !path.ends_with('/')
        && !path.contains("//")
        && !path.chars().any(char::is_control)
        && p.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

impl Operation {
    /// Whether this needs root: everything but a build installed into the
    /// person's own home.
    pub fn root(&self) -> bool {
        match self {
            Self::Packages { .. } => true,
            Self::Source { prefix, .. } => SYSTEM_PREFIXES.contains(&prefix.as_str()),
        }
    }

    /// Everything about this that can be checked without looking at the
    /// machine. `home` is the one home directory a build may be installed
    /// under, and `None` where no home directory is acceptable — which is
    /// what the root worker passes.
    pub fn validate(&self, home: Option<&Path>) -> Result<()> {
        match self {
            Self::Packages { targets } => {
                if targets.is_empty() || targets.len() > FAMILY.len() {
                    bail!("Invalid release targets");
                }
                let mut repos = BTreeSet::new();
                for target in targets {
                    let project = project(&target.repo).context("Unknown release project")?;
                    if !repos.insert(&target.repo)
                        || !valid_tag(&target.tag)
                        || target.packages.is_empty()
                    {
                        bail!("Invalid release target");
                    }
                    let mut names = BTreeSet::new();
                    for package in &target.packages {
                        if !project.packages.contains(&package.as_str()) || !names.insert(package) {
                            bail!("{package} is not a package of {}", project.repo);
                        }
                    }
                }
            }
            Self::Source {
                repo,
                tag,
                prefix,
                components,
                libdir,
                sitedir,
            } => {
                let project = project(repo).context("Unknown release project")?;
                if !valid_tag(tag) {
                    bail!("Invalid release tag");
                }
                let own = home.map(|h| h.join(".local"));
                if !SYSTEM_PREFIXES.contains(&prefix.as_str())
                    && own.as_deref() != Some(Path::new(prefix))
                {
                    bail!("{prefix} is not somewhere a build may be installed");
                }
                if !plain(prefix) {
                    bail!("Invalid prefix");
                }
                let known: Vec<&str> = project.parts.iter().map(|p| p.component).collect();
                let mut seen = BTreeSet::new();
                if components.is_empty()
                    || components
                        .iter()
                        .any(|c| !known.contains(&c.as_str()) || !seen.insert(c))
                {
                    bail!("Invalid components for {repo}");
                }
                let marked =
                    |kind: fn(&Marker) -> bool| project.parts.iter().any(|p| kind(&p.marker));
                for (dir, allowed) in [
                    (libdir, marked(|m| matches!(m, Marker::Library(_)))),
                    (sitedir, marked(|m| matches!(m, Marker::Python(_)))),
                ] {
                    if let Some(dir) = dir {
                        if !allowed || !plain(dir) || !Path::new(dir).starts_with(prefix) {
                            bail!("Invalid directory for {repo}");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The step that carries this out: this helper, handed the operation.
    pub fn step(&self) -> Result<process::Step> {
        Ok(process::Step::of_helper(
            vec!["release".into(), serde_json::to_string(self)?],
            self.root(),
        ))
    }
}

// ───────────────────────────── the check ─────────────────────────────

/// Ask every project this source updates what it has released since, and
/// keep the answer as items — one a package, or one a project built from
/// source — and as the operations Update now would carry out.
pub fn check(source: &mut crate::Source) -> Result<()> {
    source.excluded.clear();
    let host = Host::read();
    let format = Format::of(&host, host.system());
    let plan = plan(
        &find(&host),
        format,
        &format.map(Format::arch).unwrap_or_default(),
        releases,
        built_version,
    )?;
    let home = home();
    for operation in &plan.operations {
        operation.validate(home.as_deref())?;
    }
    let building = plan
        .operations
        .iter()
        .any(|o| matches!(o, Operation::Source { .. }));
    source.provider = crate::Provider::Releases {
        operations: plan.operations,
    };
    source.items = plan.items;
    source.listed = true;
    source.fresh = true;
    source.note = if building {
        "Released on GitHub · built from source here, which takes a while".into()
    } else {
        "Released on GitHub · for what this system's repositories do not carry".into()
    };
    for repo in plan.missing {
        source.excluded.push(Item {
            name: repo.into(),
            detail: "Its newest release has nothing this system can install, or its installed version could not be read".into(),
        });
    }
    Ok(())
}

/// What a check decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub operations: Vec<Operation>,
    pub items: Vec<Item>,
    /// Projects a newer release was found for and that cannot be updated
    /// from it: it has no package for this system, or what is installed
    /// could not say its version.
    pub missing: Vec<&'static str>,
}

/// Decide, from what is installed and what each project has released, what
/// Update now would do. `list` is a project's release list and `version_of`
/// what a build from source is at — GitHub and the machine, in a check, and
/// anything at all in a test.
pub fn plan(
    found: &[Found],
    format: Option<Format>,
    arch: &str,
    list: impl Fn(&str) -> Result<Vec<Release>>,
    version_of: impl Fn(&Built) -> Option<Version>,
) -> Result<Plan> {
    let mut targets = vec![];
    let mut builds = vec![];
    let mut plan = Plan::default();
    for found in found {
        let repo = match found {
            Found::Packaged { repo, .. } => *repo,
            Found::Built(built) => built.repo,
        };
        let list = list(repo)?;
        let Some((release, version)) = newest(&list) else {
            continue;
        };
        match found {
            Found::Packaged { packages, .. } => {
                let Some(format) = format else { continue };
                let mut newer = vec![];
                let mut absent = false;
                for (name, installed) in packages {
                    match package_for(format, arch, &release.assets, name) {
                        Some(package)
                            if vercmp(&package.version, installed) == Ordering::Greater =>
                        {
                            newer.push((name.clone(), installed.clone(), package))
                        }
                        Some(_) => {}
                        None => absent |= older_than(installed, &version),
                    }
                }
                // All of a project or none of it: its packages are
                // version-locked to one another, and a release that brings
                // some of them and not the rest is one the package manager
                // would refuse half-way.
                if absent {
                    plan.missing.push(repo);
                    continue;
                }
                if newer.is_empty() {
                    continue;
                }
                for (name, installed, package) in &newer {
                    plan.items.push(Item {
                        name: name.clone(),
                        detail: format!("{installed} → {}", package.version),
                    });
                }
                targets.push(Target {
                    repo: repo.to_owned(),
                    tag: release.tag_name.clone(),
                    packages: newer.into_iter().map(|(name, ..)| name).collect(),
                });
            }
            Found::Built(built) => {
                let Some(installed) = version_of(built) else {
                    plan.missing.push(repo);
                    continue;
                };
                if version <= installed {
                    continue;
                }
                plan.items.push(Item {
                    name: repo.to_owned(),
                    detail: format!("{installed} → {version} · built from source"),
                });
                builds.push(Operation::Source {
                    repo: repo.to_owned(),
                    tag: release.tag_name.clone(),
                    prefix: built.prefix.to_string_lossy().into_owned(),
                    components: built.components.iter().map(|c| (*c).to_owned()).collect(),
                    libdir: built
                        .libdir
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    sitedir: built
                        .sitedir
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                });
            }
        }
    }
    // The packages first, in one transaction, and then the builds in the
    // family's order: an application built from source compiles the
    // toolkit's crates in, so a toolkit that comes as a package has to be
    // the new one before that build starts.
    if !targets.is_empty() {
        plan.operations.push(Operation::Packages { targets });
    }
    plan.operations.extend(builds);
    Ok(plan)
}

/// Whether a package installed at `installed` is older than the release
/// `version` — read by the package's own version, which is the release's
/// numbers with a package release after them.
fn older_than(installed: &str, version: &Version) -> bool {
    let upstream = installed
        .split_once(':')
        .map_or(installed, |(_, v)| v)
        .rsplit_once('-')
        .map_or(installed, |(v, _)| v);
    Version::parse(upstream).is_none_or(|v| v.numbers < version.numbers)
}

/// The steps Update now runs for this source, in order.
pub fn steps(operations: &[Operation]) -> Result<Vec<process::Step>> {
    operations.iter().map(Operation::step).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_tag_reads_as_the_version_it_names() {
        assert_eq!(
            v("v0.9.0-alpha"),
            Version {
                numbers: vec![0, 9, 0],
                pre: vec!["alpha".into()]
            }
        );
        assert!(v("v0.9.0-alpha") < v("0.9.0"));
        assert!(v("0.9.0") < v("0.9.1"));
        assert!(v("0.9.1-alpha") > v("0.9.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-beta"));
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert_eq!(v("0.9"), v("0.9.0"));
        assert_eq!(v("0.9.0").to_string(), "0.9.0");
        assert_eq!(v("v1.2.3-rc.1").to_string(), "1.2.3-rc.1");
        for bad in ["", "v", "latest", "0..1", "1.2.3.4.5", "1.2-", "nightly-1"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn package_versions_order_as_the_package_managers_order_them() {
        assert_eq!(vercmp("0.9.0-1", "0.9.1-1"), Ordering::Less);
        assert_eq!(vercmp("0.9.10-1", "0.9.9-1"), Ordering::Greater);
        assert_eq!(vercmp("0.9.0-2", "0.9.0-1"), Ordering::Greater);
        assert_eq!(vercmp("0.9.0-1.fc44", "0.9.0-1.fc44"), Ordering::Equal);
        assert_eq!(vercmp("0.9.0-1.fc45", "0.9.0-1.fc44"), Ordering::Greater);
        assert_eq!(vercmp("1:0.1.0-1", "0.9.0-1"), Ordering::Greater);
        assert_eq!(vercmp("1.0~rc1-1", "1.0-1"), Ordering::Less);
        assert_eq!(vercmp("1.0a-1", "1.0-1"), Ordering::Greater);
        assert_eq!(vercmp("1.0.1-1", "1.0a-1"), Ordering::Greater);
    }

    #[test]
    fn what_a_program_says_is_read_for_its_version() {
        assert_eq!(reported("lxb-updates 0.9.1\n"), Some(v("0.9.1")));
        assert_eq!(reported("cedm 0.9.0"), Some(v("0.9.0")));
        assert_eq!(reported(" 0.9.0"), Some(v("0.9.0")));
        assert_eq!(reported("usage: something"), None);
    }

    /// The release every project published on 2026-09-23, as GitHub lists
    /// it — the shape that matters, with fewer files.
    const LISTED: &str = r#"[
      {"tag_name":"v0.9.0-alpha","draft":false,"prerelease":true,"assets":[
        {"name":"lxb-compositor-0.9.0-1-x86_64.pkg.tar.zst","size":10,"digest":"sha256:aa","browser_download_url":"https://github.com/Petexy/LineXinBar/releases/download/v0.9.0-alpha/lxb-compositor-0.9.0-1-x86_64.pkg.tar.zst"},
        {"name":"lxb-compositor-0.9.0-1.fc44.aarch64.rpm","size":10,"browser_download_url":"https://github.com/Petexy/LineXinBar/releases/download/v0.9.0-alpha/lxb-compositor-0.9.0-1.fc44.aarch64.rpm"},
        {"name":"lxb-compositor-0.9.0-1.fc44.x86_64.rpm","size":10,"browser_download_url":"https://github.com/Petexy/LineXinBar/releases/download/v0.9.0-alpha/lxb-compositor-0.9.0-1.fc44.x86_64.rpm"},
        {"name":"lxb-compositor-0.9.0-1.fc42.x86_64.rpm","size":10,"browser_download_url":"https://github.com/Petexy/LineXinBar/releases/download/v0.9.0-alpha/lxb-compositor-0.9.0-1.fc42.x86_64.rpm"},
        {"name":"lxb-compositor_0.9.0-1_amd64.deb","size":10,"browser_download_url":"https://github.com/Petexy/LineXinBar/releases/download/v0.9.0-alpha/lxb-compositor_0.9.0-1_amd64.deb"},
        {"name":"python3-lxb-toolkit_0.9.0-1_all.deb","size":10,"browser_download_url":"https://github.com/Petexy/lxb-toolkit/releases/download/v0.9.0-alpha/python3-lxb-toolkit_0.9.0-1_all.deb"}
      ]},
      {"tag_name":"v1.0.0-beta","draft":true,"prerelease":true,"assets":[]},
      {"tag_name":"nightly","draft":false,"prerelease":true,"assets":[]}
    ]"#;

    #[test]
    fn the_newest_release_is_the_highest_published_version() {
        let list: Vec<Release> = serde_json::from_str(LISTED).unwrap();
        let (release, version) = newest(&list).unwrap();
        assert_eq!(release.tag_name, "v0.9.0-alpha", "a draft is never newest");
        assert_eq!(version, v("0.9.0-alpha"));
    }

    #[test]
    fn each_format_takes_its_own_file_for_its_own_machine() {
        let list: Vec<Release> = serde_json::from_str(LISTED).unwrap();
        let assets = &list[0].assets;
        let arch = |a: &str| package_for(Format::Pacman, a, assets, "lxb-compositor");
        assert_eq!(arch("x86_64").unwrap().version, "0.9.0-1");
        assert_eq!(arch("x86_64").unwrap().digest.as_deref(), Some("sha256:aa"));
        assert!(
            arch("aarch64").is_none(),
            "no Arch build for ARM was attached"
        );
        let deb = package_for(Format::Deb, "amd64", assets, "lxb-compositor").unwrap();
        assert_eq!(deb.file, "lxb-compositor_0.9.0-1_amd64.deb");
        assert!(package_for(Format::Deb, "arm64", assets, "lxb-compositor").is_none());
        assert_eq!(
            package_for(Format::Deb, "arm64", assets, "python3-lxb-toolkit")
                .unwrap()
                .version,
            "0.9.0-1",
            "an _all package fits every architecture"
        );
        let fedora = |n, a: &str| {
            package_for(
                Format::Rpm {
                    fedora: n,
                    dnf5: true,
                },
                a,
                assets,
                "lxb-compositor",
            )
        };
        assert_eq!(fedora(44, "x86_64").unwrap().version, "0.9.0-1.fc44");
        assert_eq!(fedora(45, "x86_64").unwrap().version, "0.9.0-1.fc44");
        assert_eq!(fedora(43, "x86_64").unwrap().version, "0.9.0-1.fc42");
        assert!(
            fedora(41, "x86_64").is_none(),
            "never one built for a newer Fedora"
        );
        assert_eq!(
            fedora(44, "aarch64").unwrap().file,
            "lxb-compositor-0.9.0-1.fc44.aarch64.rpm"
        );
        assert!(package_for(Format::Pacman, "x86_64", assets, "lxb-desktop").is_none());
    }

    #[test]
    fn a_hand_installed_deb_is_the_only_one_apt_knows_of() {
        let policy = "lxb-desktop:\n  Installed: 0.9.0-1\n  Candidate: 0.9.0-1\n  Version table:\n *** 0.9.0-1 100\n        100 /var/lib/dpkg/status\nlxb-compositor:\n  Installed: 0.9.0-1\n  Candidate: 0.9.2-1\n  Version table:\n     0.9.2-1 500\n        500 http://deb.example.org/debian trixie/main amd64 Packages\n *** 0.9.0-1 100\n        100 /var/lib/dpkg/status\n";
        assert!(!apt_carries(policy, "lxb-desktop"));
        assert!(apt_carries(policy, "lxb-compositor"));
        assert!(!apt_carries(policy, "cedm"));
    }

    #[test]
    fn portage_is_asked_through_what_each_package_installed() {
        let root = std::env::temp_dir().join(format!(
            "lxb-portage-{}-{}",
            std::process::id(),
            crate::now()
        ));
        let package = root.join("gui-wm/linexinbar-0.9.0");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("CONTENTS"),
            "dir /usr/bin\nobj /usr/bin/lxb 0123456789abcdef0123456789abcdef 1790000000\nsym /usr/bin/lxb-session -> lxb 1790000000\n",
        )
        .unwrap();
        assert_eq!(portage_owns(&root, "/usr/bin/lxb"), Some(true));
        assert_eq!(portage_owns(&root, "/usr/bin/lxb-session"), Some(true));
        assert_eq!(portage_owns(&root, "/usr/bin/lxb-desktop"), Some(false));
        assert_eq!(portage_owns(&root, "/usr/bin/lx"), Some(false));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_build_under_a_prefix_is_found_by_what_it_installed() {
        let prefix = std::env::temp_dir().join(format!(
            "lxb-prefix-{}-{}",
            std::process::id(),
            crate::now()
        ));
        let toolkit = project("lxb-toolkit").unwrap();
        assert!(built(toolkit, &prefix, System::Unknown).is_none());
        std::fs::create_dir_all(prefix.join("lib64/pkgconfig")).unwrap();
        std::fs::write(prefix.join("lib64/liblxb_toolkit.so"), "").unwrap();
        std::fs::write(
            prefix.join("lib64/pkgconfig/lxb-toolkit.pc"),
            "Name: lxb-toolkit\nVersion: 0.9.0\n",
        )
        .unwrap();
        std::fs::create_dir_all(prefix.join("lib/python3.14/site-packages/lxb_toolkit")).unwrap();
        let found = built(toolkit, &prefix, System::Unknown).unwrap();
        assert_eq!(found.components, ["library", "python"]);
        assert_eq!(found.libdir, Some(prefix.join("lib64")));
        assert_eq!(
            found.sitedir,
            Some(prefix.join("lib/python3.14/site-packages"))
        );
        assert_eq!(built_version(&found), Some(v("0.9.0")));
        std::fs::create_dir_all(prefix.join("share/lxb-updates/installed")).unwrap();
        std::fs::write(
            record_path(&prefix, "lxb-toolkit"),
            r#"{"tag":"v0.9.0-alpha","files":[]}"#,
        )
        .unwrap();
        assert_eq!(
            built_version(&found),
            Some(v("0.9.0-alpha")),
            "the tag the updater installed is more exact than the number the files say"
        );
        std::fs::remove_dir_all(prefix).unwrap();
    }

    /// Under /usr, what a package manager owns is not a build — and on a
    /// system this cannot ask, nothing there is taken for one.
    #[test]
    fn under_usr_a_build_needs_a_package_manager_to_disown_it() {
        let usr = Path::new("/usr");
        let cedm = project("CEDM").unwrap();
        if !usr.join("bin/cedm").exists() {
            assert!(built(cedm, usr, System::Unknown).is_none());
        }
        assert_eq!(owned(System::Unknown, Path::new("/usr/bin/cedm")), None);
        assert_eq!(owned(System::Xbps, Path::new("/usr/bin/cedm")), None);
    }

    #[test]
    fn an_operation_names_only_family_things_in_the_places_allowed() {
        let packages = |repo: &str, tag: &str, names: &[&str]| Operation::Packages {
            targets: vec![Target {
                repo: repo.into(),
                tag: tag.into(),
                packages: names.iter().map(|n| (*n).into()).collect(),
            }],
        };
        assert!(
            packages("LineXinBar", "v0.9.2", &["lxb-desktop", "lxb-compositor"])
                .validate(None)
                .is_ok()
        );
        assert!(packages("LineXinBar", "v0.9.2", &["cedm"])
            .validate(None)
            .is_err());
        assert!(packages("Other", "v0.9.2", &["x"]).validate(None).is_err());
        assert!(packages("LineXinBar", "--upload-pack=x", &["lxb-desktop"])
            .validate(None)
            .is_err());
        assert!(packages("LineXinBar", "v0.9.2/../x", &["lxb-desktop"])
            .validate(None)
            .is_err());
        assert!(packages("LineXinBar", "main", &["lxb-desktop"])
            .validate(None)
            .is_err());
        assert!(packages("LineXinBar", "v0.9.2", &[])
            .validate(None)
            .is_err());

        let source = |repo: &str, prefix: &str, components: &[&str], libdir: Option<&str>| {
            Operation::Source {
                repo: repo.into(),
                tag: "v0.9.2".into(),
                prefix: prefix.into(),
                components: components.iter().map(|c| (*c).into()).collect(),
                libdir: libdir.map(Into::into),
                sitedir: None,
            }
        };
        let home = Path::new("/home/someone");
        assert!(
            source("LineXinBar", "/usr", &["compositor", "desktop"], None)
                .validate(None)
                .is_ok()
        );
        assert!(source("LineXinBar", "/usr/local", &["desktop"], None)
            .validate(None)
            .is_ok());
        assert!(source("LineXinBar", "/opt", &["desktop"], None)
            .validate(None)
            .is_err());
        assert!(source("LineXinBar", "/usr", &["everything"], None)
            .validate(None)
            .is_err());
        assert!(source("DistriBumpy", "/usr", &[""], None)
            .validate(None)
            .is_ok());
        assert!(
            source("DistriBumpy", "/home/someone/.local", &[""], None)
                .validate(None)
                .is_err(),
            "never a home directory as root"
        );
        assert!(source("DistriBumpy", "/home/someone/.local", &[""], None)
            .validate(Some(home))
            .is_ok());
        assert!(source("DistriBumpy", "/home/other/.local", &[""], None)
            .validate(Some(home))
            .is_err());
        assert!(
            source("lxb-toolkit", "/usr", &["library"], Some("/usr/lib64"))
                .validate(None)
                .is_ok()
        );
        assert!(source(
            "lxb-toolkit",
            "/usr",
            &["library"],
            Some("/usr/lib/../../etc")
        )
        .validate(None)
        .is_err());
        assert!(
            source("lxb-toolkit", "/usr/local", &["library"], Some("/usr/lib"))
                .validate(None)
                .is_err(),
            "a libdir outside its prefix"
        );
        assert!(
            source("CEDM", "/usr", &[""], Some("/usr/lib"))
                .validate(None)
                .is_err(),
            "no libdir for a project with no library"
        );

        assert!(serde_json::from_str::<Operation>(
            r#"{"kind":"packages","targets":[],"command":"sh"}"#
        )
        .is_err());
        let step = source("LineXinBar", "/usr", &["desktop"], None)
            .step()
            .unwrap();
        assert!(step.helper && step.root);
        assert_eq!(step.args[0], "release");
        let user = source("DistriBumpy", "/home/someone/.local", &[""], None)
            .step()
            .unwrap();
        assert!(
            !user.root,
            "a build into the person's own home needs nobody's password"
        );
    }

    /// A machine with the shell installed from a release page and Songonsole
    /// built into /usr/local, and a new release of both: the packages in one
    /// transaction, then the build.
    #[test]
    fn a_newer_release_becomes_one_transaction_and_then_the_builds() {
        let release = |tag: &str, files: &[&str]| {
            let assets: Vec<serde_json::Value> = files
                .iter()
                .map(|f| serde_json::json!({"name": f, "size": 1, "digest": format!("sha256:{}", "0".repeat(64)),
                    "browser_download_url": format!("https://github.com/Petexy/LineXinBar/releases/download/{tag}/{f}")}))
                .collect();
            serde_json::json!([{"tag_name": tag, "draft": false, "prerelease": true, "assets": assets}]).to_string()
        };
        let shell = release(
            "v0.9.2-alpha",
            &[
                "lxb-compositor-0.9.2-1-x86_64.pkg.tar.zst",
                "lxb-desktop-0.9.2-1-x86_64.pkg.tar.zst",
                "lxb-retroarch-0.9.2-1-x86_64.pkg.tar.zst",
            ],
        );
        let music = release("v0.9.2-alpha", &[]);
        let list = |repo: &str| -> Result<Vec<Release>> {
            Ok(serde_json::from_str(match repo {
                "LineXinBar" => &shell,
                "SongOnSole" => &music,
                _ => bail!("no such fixture"),
            })?)
        };
        let packaged = Found::Packaged {
            repo: "LineXinBar",
            packages: vec![
                ("lxb-compositor".into(), "0.9.0-1".into()),
                ("lxb-desktop".into(), "0.9.0-1".into()),
            ],
        };
        let music_build = Built {
            repo: "SongOnSole",
            prefix: "/usr/local".into(),
            components: vec![""],
            libdir: None,
            sitedir: None,
        };
        let found = [packaged.clone(), Found::Built(music_build.clone())];
        let planned = plan(&found, Some(Format::Pacman), "x86_64", list, |_| {
            Some(v("0.9.0"))
        })
        .unwrap();
        assert_eq!(
            planned.operations,
            [
                Operation::Packages {
                    targets: vec![Target {
                        repo: "LineXinBar".into(),
                        tag: "v0.9.2-alpha".into(),
                        packages: vec!["lxb-compositor".into(), "lxb-desktop".into()],
                    }]
                },
                Operation::Source {
                    repo: "SongOnSole".into(),
                    tag: "v0.9.2-alpha".into(),
                    prefix: "/usr/local".into(),
                    components: vec![String::new()],
                    libdir: None,
                    sitedir: None,
                }
            ],
            "only what is installed, and never the retroarch package nobody asked for"
        );
        assert_eq!(
            planned
                .items
                .iter()
                .map(|i| (i.name.as_str(), i.detail.as_str()))
                .collect::<Vec<_>>(),
            [
                ("lxb-compositor", "0.9.0-1 → 0.9.2-1"),
                ("lxb-desktop", "0.9.0-1 → 0.9.2-1"),
                ("SongOnSole", "0.9.0 → 0.9.2-alpha · built from source"),
            ]
        );
        for operation in &planned.operations {
            operation.validate(None).unwrap();
        }

        // A development build newer than anything released is up to date,
        // not something to take back to the release.
        let ahead = Found::Packaged {
            repo: "LineXinBar",
            packages: vec![("lxb-desktop".into(), "0.9.3-1".into())],
        };
        let plan_ahead = plan(&[ahead], Some(Format::Pacman), "x86_64", list, |_| None).unwrap();
        assert_eq!(plan_ahead, Plan::default());
        let built_ahead = plan(&[Found::Built(music_build.clone())], None, "", list, |_| {
            Some(v("0.9.3"))
        })
        .unwrap();
        assert_eq!(built_ahead, Plan::default());

        // A newer release without a package this machine can install is
        // said, and not installed in part.
        let heroic = Found::Packaged {
            repo: "LineXinBar",
            packages: vec![
                ("lxb-desktop".into(), "0.9.0-1".into()),
                ("lxb-heroic".into(), "0.9.0-1".into()),
            ],
        };
        let partial = plan(&[heroic], Some(Format::Pacman), "x86_64", list, |_| None).unwrap();
        assert!(partial.operations.is_empty());
        assert_eq!(partial.missing, ["LineXinBar"]);
        let arm = plan(&[packaged], Some(Format::Pacman), "aarch64", list, |_| None).unwrap();
        assert_eq!(arm.missing, ["LineXinBar"]);
        let unreadable = plan(&[Found::Built(music_build)], None, "", list, |_| None).unwrap();
        assert_eq!(unreadable.missing, ["SongOnSole"]);
    }

    #[test]
    fn a_package_missing_from_a_newer_release_is_told_apart_from_an_older_one() {
        assert!(older_than("0.9.0-1", &v("v0.9.2")));
        assert!(!older_than("0.9.1-1", &v("v0.9.0-alpha")));
        assert!(!older_than("0.9.0-1.fc44", &v("v0.9.0")));
        assert!(older_than("1:0.8.0-1", &v("v0.9.0")));
    }
}
