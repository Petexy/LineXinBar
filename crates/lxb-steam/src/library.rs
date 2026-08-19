//! The library: everything the account owns, and which of it is on this disk.
//!
//! Two halves that answer two different questions, joined here into one list.
//!
//! **What is owned** comes from the account's live Steam Connection Manager
//! session. Steam sends the packages that account is licensed for; their PICS
//! records resolve those packages into games, names and launch entries. A
//! token granted by the QR service is a CM logon credential, not a Web API key.
//!
//! **What is installed** comes off the disk, and cannot come from anywhere
//! else: Steam does not know what is on a particular machine, only what an
//! account has downloaded somewhere. The libraries are listed in
//! `steamapps/libraryfolders.vdf` and each installed game has an
//! `appmanifest_<id>.acf` beside it — the same files the Steam client itself
//! reads, so the two agree by construction rather than by this crate keeping
//! its own record and hoping.
//!
//! Reading the disk is also what makes the column right on a machine that has
//! never signed in through this shell at all. Somebody who installed a game in
//! Steam an hour ago sees it as installed the moment they sign in here,
//! because the file that says so is already there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::vdf;

/// One way Steam says an installed title can be started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    pub executable: String,
    pub arguments: Option<String>,
    pub working_dir: Option<String>,
    pub kind: Option<String>,
    pub operating_systems: Option<String>,
    pub architecture: Option<String>,
    pub beta_key: Option<String>,
}

/// One title in the account's library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    pub app_id: u32,
    pub name: String,
    /// Whether it is on this machine's disk now.
    pub installed: bool,
    /// What it takes up, as the manifest says. Zero for anything not
    /// installed: Steam does not say how big a game is until it is being
    /// downloaded, so the honest answer for the rest of the library is nothing
    /// rather than a guess.
    pub size_on_disk: u64,
    /// Where it is, for the ones that are somewhere.
    pub install_path: Option<PathBuf>,
    /// How long the account has played it, in minutes, ever.
    pub playtime_minutes: u32,
    /// When the account last played it, as Steam counts seconds. Zero for a
    /// game that has never been started — Steam lists last-played times for the
    /// games that have one and says nothing about the rest, so there is no date
    /// to be had rather than a date of nineteen seventy.
    pub last_played: u32,
    /// Whether Steam is in the middle of downloading or updating it. Such a
    /// game is not yet playable and is listed with the ones that are not
    /// installed, which is where the user will look for it while they wait.
    pub updating: bool,
    /// How far the client has got fetching it, in bytes, while it is being
    /// fetched. Both zero otherwise — and briefly while it starts, because the
    /// client writes the manifest before it knows the size.
    pub downloaded: u64,
    pub to_download: u64,
    /// The folder name Steam assigns below `steamapps/common`.
    pub install_dir: Option<String>,
    /// Launch choices resolved from the app's PICS `config/launch` block.
    pub launch: Vec<Launch>,
    /// Where Steam publishes this game's cover, hero and logo, from the same
    /// PICS record. Empty for a game that is on the disk without being in this
    /// account's catalogue — see [`crate::art::Published`].
    pub pictures: crate::art::Published,
    /// What the row is sorted on: the name, folded. Kept rather than computed
    /// at every comparison, because a library of a thousand games is sorted
    /// whenever one of them finishes installing.
    order: String,
}

impl Game {
    fn new(app_id: u32, name: String, playtime_minutes: u32) -> Game {
        Game {
            order: sort_key(&name),
            app_id,
            name,
            installed: false,
            size_on_disk: 0,
            install_path: None,
            playtime_minutes,
            last_played: 0,
            updating: false,
            downloaded: 0,
            to_download: 0,
            install_dir: None,
            launch: Vec::new(),
            pictures: crate::art::Published::default(),
        }
    }

    /// A title that came from nowhere: for `--debug-steam-library`, which is
    /// how the Steam column is looked at without an account.
    ///
    /// Here rather than in the shell because [`Game`] keeps the key it sorts
    /// on, and a fixture built field by field outside this module would be one
    /// that sorted differently from the real thing — which is exactly what the
    /// fixture exists to check.
    pub fn invented(app_id: u32, name: String, installed: bool) -> Game {
        let mut game = Game::new(app_id, name, 0);
        game.installed = installed;
        game.size_on_disk = u64::from(app_id) * 1_000_000_000;
        game
    }

    /// What goes under the name on the bar.
    ///
    /// The one line the row has to say something in, so it says the thing that
    /// differs between the two halves of the column: whether this is a game
    /// that can be started right now.
    pub fn note(&self) -> String {
        if self.updating {
            // A percentage only once there is one: the client writes the
            // manifest before it knows the size, and "Downloading 0%" for the
            // first few seconds of every install reads as a stall.
            return match (self.downloaded, self.to_download) {
                (_, 0) => "Downloading".to_string(),
                (done, total) => format!(
                    "Downloading · {:.0}% of {}",
                    (done as f64 / total as f64) * 100.0,
                    size_said(total)
                ),
            };
        }
        if !self.installed {
            return "Not installed".to_string();
        }
        match (self.size_on_disk, self.playtime_minutes) {
            (0, _) => "Installed".to_string(),
            (size, 0) => format!("Installed · {}", size_said(size)),
            (size, played) => {
                format!(
                    "Installed · {} · {} played",
                    size_said(size),
                    time_said(played)
                )
            }
        }
    }
}

/// How the column is ordered unless somebody says otherwise: installed first,
/// each half by name.
///
/// The order the user asked for, and the order that answers the question they
/// are actually asking — what can I play — before the one they are not. Within
/// each half it is by name, folded, so `The Witness` and `the witness` are not
/// two different places in the list, and ties are broken by the app id so that
/// two games with the same name never swap places between one refresh and the
/// next.
///
/// This is what every library arrives in, whichever order it is then listed in:
/// [`Sort::InstalledFirst`] costs nothing because the list is already in it.
pub fn sorted(games: Vec<Game>) -> Vec<Game> {
    sorted_by(games, Sort::InstalledFirst)
}

/// The same, in whichever order was asked for.
pub fn sorted_by(mut games: Vec<Game>, sort: Sort) -> Vec<Game> {
    games.sort_by(|a, b| sort.compare(a, b));
    games
}

/// The order a Steam library is listed in.
///
/// The questions somebody asks of a library of games, which are not the
/// questions they ask of a folder of their own files: what can I play now, what
/// is it called, when did I last play it, how much of my life has it had, and
/// what is it costing me in disk. There is no Type here and no Created, and
/// there are three the shell's shelves of music and photographs have nothing to
/// answer with.
///
/// Each of the three that has a direction is two orders rather than one order
/// with a flag, for the reason the shell's menu commands are one per row: what
/// the user picks is a row with a name on it, and "largest first" and "smallest
/// first" are two names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// Everything that can be played now, then everything else, each half by
    /// name. The one the library is kept in, and so the only one that costs
    /// nothing — see [`sorted`].
    #[default]
    InstalledFirst,
    /// Straight down the alphabet, installed or not.
    ///
    /// Deliberately *not* installed-first-then-by-name, which is what
    /// [`Sort::InstalledFirst`] already is. Somebody who picks this row is
    /// looking a game up by its name, and a list that put half the alphabet
    /// through twice would be the harder of the two to look anything up in.
    NameAscending,
    NameDescending,
    RecentlyPlayedFirst,
    MostPlayedFirst,
    LeastPlayedFirst,
    LargestFirst,
    SmallestFirst,
}

/// Every order, in the one place that decides what the Sort menu looks like:
/// the rows are built from this, so a new order is a variant and a line here.
pub const SORTS: &[Sort] = &[
    Sort::InstalledFirst,
    Sort::NameAscending,
    Sort::NameDescending,
    Sort::RecentlyPlayedFirst,
    Sort::MostPlayedFirst,
    Sort::LeastPlayedFirst,
    Sort::LargestFirst,
    Sort::SmallestFirst,
];

/// What a library has to be sorted *by*, as against what it could be asked for.
///
/// Steam answers three questions about a game separately from the catalogue
/// itself, and any of the three can come back with nothing: a machine with no
/// game installed knows no sizes, and the last-played times are a second
/// request that is allowed to fail without taking the library down with it —
/// see `cm::library`. An order nothing can be sorted by is offered greyed
/// rather than left out, so the menu keeps its shape and says out loud what
/// this account cannot be asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Orders {
    /// Whether anything here is on the disk, which is the only way a size is
    /// known.
    pub sizes: bool,
    /// Whether anything here has been played at all.
    pub playtimes: bool,
    /// Whether anything here carries the date it was last played.
    pub played: bool,
}

/// What this library can be sorted by.
pub fn orders(games: &[Game]) -> Orders {
    Orders {
        sizes: games.iter().any(|game| game.size_on_disk > 0),
        playtimes: games.iter().any(|game| game.playtime_minutes > 0),
        played: games.iter().any(|game| game.last_played > 0),
    }
}

impl Sort {
    /// What the row that chooses it says.
    ///
    /// Kept short, and to the pattern the shell's other Sort list already uses.
    /// The menu panel is a fixed width and cuts a label that runs past it with
    /// an ellipsis, so a row named at length would be the one row of the list
    /// nobody can read — and it would be cut *worse* than its neighbours,
    /// because the row in force wears a tick that takes a further glyph's width
    /// off the label. "Last played (most recent first)" was the first draft of
    /// the fourth of these and did not fit; it has only one direction to be in,
    /// so it does not need one saying.
    pub fn label(self) -> &'static str {
        match self {
            Sort::InstalledFirst => "Installed first",
            Sort::NameAscending => "Name (A to Z)",
            Sort::NameDescending => "Name (Z to A)",
            Sort::RecentlyPlayedFirst => "Recently played",
            Sort::MostPlayedFirst => "Play time (most first)",
            Sort::LeastPlayedFirst => "Play time (least first)",
            Sort::LargestFirst => "Size (largest first)",
            Sort::SmallestFirst => "Size (smallest first)",
        }
    }

    /// What it is called in the settings file.
    ///
    /// Written out rather than derived from the variant name, because it is a
    /// thing a user may open a text editor and read: the file is theirs, and
    /// `size-largest-first` says what it does where `LargestFirst` says what a
    /// Rust enum is called. The two orders this shares a name with on a shelf of
    /// files are spelled the same way on purpose — one file, one vocabulary.
    pub fn key(self) -> &'static str {
        match self {
            Sort::InstalledFirst => "installed-first",
            Sort::NameAscending => "name",
            Sort::NameDescending => "name-reversed",
            Sort::RecentlyPlayedFirst => "last-played",
            Sort::MostPlayedFirst => "play-time-most-first",
            Sort::LeastPlayedFirst => "play-time-least-first",
            Sort::LargestFirst => "size-largest-first",
            Sort::SmallestFirst => "size-smallest-first",
        }
    }

    /// The order of that name, or `None` for a file that names one this shell
    /// does not have — a hand-edited typo, or a setting from a later version.
    pub fn from_key(key: &str) -> Option<Sort> {
        SORTS.iter().copied().find(|sort| sort.key() == key)
    }

    /// Whether a library that knows these things has anything to sort by in
    /// this order. See [`Orders`].
    pub fn orders(self, knows: Orders) -> bool {
        match self {
            Sort::LargestFirst | Sort::SmallestFirst => knows.sizes,
            Sort::MostPlayedFirst | Sort::LeastPlayedFirst => knows.playtimes,
            Sort::RecentlyPlayedFirst => knows.played,
            Sort::InstalledFirst | Sort::NameAscending | Sort::NameDescending => true,
        }
    }

    /// Put `a` before `b`, or after it.
    pub fn compare(self, a: &Game, b: &Game) -> std::cmp::Ordering {
        // The name settles every other order in here, so two games of the same
        // size are in the order the user already knows rather than in whichever
        // order Steam happened to send them; and the app id settles that, so
        // that two games with the same name never swap places between one
        // refresh and the next.
        let then = || a.order.cmp(&b.order).then_with(|| a.app_id.cmp(&b.app_id));
        match self {
            // A game being downloaded is not one that can be played, so it
            // sorts with the ones that are not installed.
            Sort::InstalledFirst => {
                let ready = |game: &Game| game.installed && !game.updating;
                ready(b).cmp(&ready(a)).then_with(then)
            }
            Sort::NameAscending => then(),
            Sort::NameDescending => b
                .order
                .cmp(&a.order)
                // The tie-break is *not* reversed with the order. Two games of
                // one name are one row apart wherever the list is read from,
                // and the id is there to hold them still rather than to be
                // read.
                .then_with(|| a.app_id.cmp(&b.app_id)),
            Sort::RecentlyPlayedFirst => {
                by_known(a.last_played.into(), b.last_played.into(), true).then_with(then)
            }
            // Played for no minutes is a quantity and not a gap: a library
            // sorted least-played-first is being asked "what have I not got
            // round to", and the answer is meant to be at the top.
            Sort::MostPlayedFirst => b.playtime_minutes.cmp(&a.playtime_minutes).then_with(then),
            Sort::LeastPlayedFirst => a.playtime_minutes.cmp(&b.playtime_minutes).then_with(then),
            Sort::LargestFirst => by_known(a.size_on_disk, b.size_on_disk, true).then_with(then),
            Sort::SmallestFirst => by_known(a.size_on_disk, b.size_on_disk, false).then_with(then),
        }
    }
}

/// Compare two of the numbers Steam gives for some games and not others, with
/// the game that has none always last — whichever end of the list that is.
///
/// Not the same comparison read backwards. Zero is the smallest number there
/// is, so reversing a comparison to get "largest first" would also move the
/// games nothing is known about from one end of the column to the other: they
/// would be at the head of one order and the tail of its reverse, which is not
/// what reversing an order means. A game that is not installed has no size and
/// one that has never been started has no last-played date, and neither is a
/// small one.
fn by_known(a: u64, b: u64, largest_first: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (0, 0) => Ordering::Equal,
        (0, _) => Ordering::Greater,
        (_, 0) => Ordering::Less,
        (a, b) if largest_first => b.cmp(&a),
        (a, b) => a.cmp(&b),
    }
}

/// What a name sorts as.
fn sort_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Turn the CM/PICS catalogue into this crate's deliberately smaller model.
pub fn owned(games: Vec<steam_cm_protocol::ProtocolGame>) -> Vec<Game> {
    games
        .into_iter()
        // The bar is a game library, not Steam's tools/runtime inventory.
        .filter(|game| !matches!(game.app_type.as_deref(), Some("tool")))
        .filter_map(|game| {
            let name = game.name.trim();
            if game.appid == 0 || name.is_empty() {
                return None;
            }
            let mut owned = Game::new(
                game.appid,
                name.to_string(),
                game.playtime_forever.max(0) as u32,
            );
            owned.last_played = game.rtime_last_played;
            owned.install_dir = game.installdir;
            owned.pictures = game.library_art.into();
            owned.launch = game
                .launch
                .into_iter()
                .map(|entry| Launch {
                    executable: entry.executable,
                    arguments: entry.arguments,
                    working_dir: entry.workingdir,
                    kind: entry.launch_type,
                    operating_systems: entry.oslist,
                    architecture: entry.osarch,
                    beta_key: entry.betakey,
                })
                .collect();
            Some(owned)
        })
        .collect()
}

/// Everything installed on this machine, by app id.
///
/// Read from the files the Steam client keeps, so this is what Steam itself
/// believes rather than a second record of the same thing.
pub fn installed() -> BTreeMap<u32, Installed> {
    let mut found = BTreeMap::new();
    for library in libraries() {
        let apps = library.join("steamapps");
        let Ok(entries) = std::fs::read_dir(&apps) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            if let Some(game) = read_manifest(&entry.path(), &apps) {
                // A game in two libraries at once — which happens, because a
                // library can be moved without the old manifest being removed
                // — is counted once, as the first one found.
                found.entry(game.app_id).or_insert(game);
            }
        }
    }
    found
}

/// What one `appmanifest_<id>.acf` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub app_id: u32,
    pub name: String,
    pub size_on_disk: u64,
    pub path: PathBuf,
    /// Whether this is on the disk in a state that can be played.
    pub playable: bool,
    /// Whether Steam is fetching it right now.
    pub updating: bool,
    /// How far that has got, in bytes, when it is under way. Both zero for a
    /// game that is not being fetched — and, briefly, for one that is: the
    /// client writes the manifest before it knows the size.
    pub downloaded: u64,
    pub to_download: u64,
    /// Whether this is Steam's own plumbing rather than something to play.
    /// See [`is_tool`].
    pub tool: bool,
}

/// The bits of `StateFlags` this crate reads, as Valve's own enumeration
/// numbers them.
///
/// It is a set rather than a state, and reading it as one is the mistake that
/// makes half a library look like it is downloading: a game with an update
/// waiting has `FULLY_INSTALLED | UPDATE_REQUIRED`, is on the disk, is
/// playable, and Steam patches it when it is started. What means "not yet" is
/// the installed bit being *absent*, or one of the bits that says a transfer
/// is under way right now.
mod state {
    pub const FULLY_INSTALLED: u64 = 4;
    /// Files gone or damaged: on the disk, and not playable until Steam has
    /// repaired it.
    pub const BROKEN: u64 = 32 | 128;
    pub const UNINSTALLING: u64 = 2048;
    /// Every bit that means bytes are moving: an update started, running,
    /// validating, reconfiguring, adding files, preallocating, downloading,
    /// staging or committing.
    pub const WORKING: u64 =
        1024 | 256 | 16384 | 32768 | 65536 | 131072 | 262144 | 524288 | 1048576;
}

fn read_manifest(path: &Path, steamapps: &Path) -> Option<Installed> {
    let node = vdf::parse(&std::fs::read_to_string(path).ok()?);
    let app_id = u32::try_from(node.number(&["AppState", "appid"])?).ok()?;
    let install_dir = node.string(&["AppState", "installdir"])?;
    let flags = node.number(&["AppState", "StateFlags"]).unwrap_or_default();
    let where_it_is = steamapps.join("common").join(install_dir);

    Some(Installed {
        app_id,
        name: node
            .string(&["AppState", "name"])
            .unwrap_or(install_dir)
            .to_string(),
        size_on_disk: node.number(&["AppState", "SizeOnDisk"]).unwrap_or_default(),
        downloaded: node
            .number(&["AppState", "BytesDownloaded"])
            .unwrap_or_default(),
        to_download: node
            .number(&["AppState", "BytesToDownload"])
            .unwrap_or_default(),
        playable: flags & state::FULLY_INSTALLED != 0
            && flags & (state::BROKEN | state::UNINSTALLING) == 0,
        updating: flags & state::FULLY_INSTALLED == 0 || flags & state::WORKING != 0,
        tool: is_tool(&where_it_is),
        path: where_it_is,
    })
}

/// Whether what is installed in `path` is Steam's own plumbing.
///
/// Asked of one thing only: an app that is on the disk and that the account's
/// own library did not mention. Steam installs its runtimes beside the games
/// that need them and gives each one an `appmanifest` of its own, so a column
/// built from the disk alone comes out with three versions of Proton and two
/// Linux runtimes in it — none of which anybody has ever wanted to press.
///
/// The two marks Steam itself uses, rather than a list of app ids that would
/// go stale the day Valve ships the next Proton:
///
/// * a **compatibility tool** declares itself with `toolmanifest.vdf`, which
///   is the file the Steam client finds Proton and the Linux runtimes by;
/// * the **redistributables** — the runtimes games link against — are a
///   directory holding `_CommonRedist` and nothing else.
///
/// Nothing the account owns is ever asked about, so a game that happened to
/// ship either of these is still listed: what the user owns is what the user
/// owns.
fn is_tool(path: &Path) -> bool {
    path.join("toolmanifest.vdf").is_file() || path.join("_CommonRedist").is_dir()
}

/// Fold what is on the disk into what the account owns.
///
/// Anything installed that the owned list did not mention is added to it, so
/// long as it is something to play: a game shared through a family library, or
/// one whose store page has gone, is on the disk and startable, and a column
/// that refused to list it would be disagreeing with the machine it is running
/// on. Steam's own runtimes are the thing this leaves out — see [`is_tool`].
pub fn merge(owned: Vec<Game>, installed: &BTreeMap<u32, Installed>) -> Vec<Game> {
    let mut games = owned;
    let take = |game: &mut Game, on_disk: &Installed| {
        game.installed = on_disk.playable || on_disk.updating;
        game.size_on_disk = on_disk.size_on_disk;
        game.install_path = Some(on_disk.path.clone());
        game.updating = on_disk.updating;
        game.downloaded = on_disk.downloaded;
        game.to_download = on_disk.to_download;
    };

    for game in &mut games {
        if let Some(on_disk) = installed.get(&game.app_id) {
            take(game, on_disk);
        }
    }

    let listed: std::collections::HashSet<u32> = games.iter().map(|game| game.app_id).collect();
    for (app_id, on_disk) in installed {
        if listed.contains(app_id) || on_disk.tool {
            continue;
        }
        let mut game = Game::new(*app_id, on_disk.name.clone(), 0);
        take(&mut game, on_disk);
        games.push(game);
    }

    sorted(games)
}

/// Where the Steam client keeps its libraries on this machine.
///
/// The first library is wherever Steam itself is installed; the rest are
/// listed in its own `libraryfolders.vdf`, which is how a second disk or an
/// external drive gets one. Every path is checked as it is read: a library on
/// a drive that is not plugged in today is simply not one of them.
pub fn libraries() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut add = |path: PathBuf| {
        if path.join("steamapps").is_dir() && !found.contains(&path) {
            found.push(path);
        }
    };

    let Some(root) = root() else {
        return found;
    };
    add(root.clone());

    let listing = root.join("steamapps").join("libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(&listing) else {
        return found;
    };
    let node = vdf::parse(&text);
    for (_, folder) in node.block(&["libraryfolders"]) {
        // Newer files put the path in a block per library; older ones wrote
        // the path as the value itself. Both are read, because a machine that
        // has had Steam on it for years may still have the old one.
        let path = match folder {
            vdf::Node::Value(path) => Some(path.as_str()),
            vdf::Node::Block(_) => folder.string(&["path"]),
        };
        if let Some(path) = path.filter(|path| !path.is_empty()) {
            add(PathBuf::from(path));
        }
    }
    found
}

/// Where the Steam client itself is.
///
/// The places it installs itself into on Linux, in the order it prefers them —
/// the two symbolic links it maintains first, since those follow a Steam that
/// has been moved, then the directory it unpacks into, then the Flatpak.
/// `None` when no Valve library root can be found. A future shell-managed
/// content root is deliberately a separate source rather than being guessed
/// from the presence or absence of the client executable.
pub fn root() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    native_roots(&home)
        .into_iter()
        .chain(std::iter::once(unpacks_into(&flatpak_home(&home))))
        .find(|path| looks_like_a_root(path))
}

/// The places the *native* client installs itself into, in the order it
/// prefers them.
///
/// Kept here rather than in [`crate::client`] so that the paths this module
/// searches and the paths a client is started against cannot drift apart: they
/// are the same three, read once.
pub(crate) fn native_roots(home: &Path) -> [PathBuf; 3] {
    [
        home.join(".steam").join("steam"),
        home.join(".steam").join("root"),
        unpacks_into(home),
    ]
}

/// Where a client unpacks itself below a home directory — the real one for the
/// native client, and the Flatpak's remapped one for the Flatpak.
pub(crate) fn unpacks_into(home: &Path) -> PathBuf {
    home.join(".local").join("share").join("Steam")
}

/// The home directory the Flatpak runs with, which is not the user's.
pub(crate) fn flatpak_home(home: &Path) -> PathBuf {
    home.join(".var")
        .join("app")
        .join(crate::client::FLATPAK_APP)
}

/// Whether a directory is a Steam that has been run at least once.
///
/// Deliberately not "does the directory exist": the Flatpak's remapped home is
/// made by Flatpak itself and a native root can be left behind by an install
/// that was removed, so an empty directory of the right name says nothing.
/// What says a client has been here is its own furniture.
pub fn looks_like_a_root(path: &Path) -> bool {
    path.join("steamapps").is_dir() || path.join("config").is_dir()
}

/// A size, as the row under a game's name says it.
fn size_said(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if size < 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

/// How long something has been played, in the units a person would say it in.
fn time_said(minutes: u32) -> String {
    match minutes {
        0..=59 => format!("{minutes} min"),
        _ => {
            let hours = f64::from(minutes) / 60.0;
            if hours < 10.0 {
                format!("{hours:.1} hours")
            } else {
                format!("{hours:.0} hours")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_cm_protocol::{LaunchEntry, ProtocolGame};

    fn game(app_id: u32, name: &str, installed: bool) -> Game {
        let mut game = Game::new(app_id, name.to_string(), 0);
        game.installed = installed;
        game
    }

    fn protocol_game(app_id: u32, name: &str) -> ProtocolGame {
        ProtocolGame {
            appid: app_id,
            name: name.to_string(),
            playtime_forever: 0,
            rtime_last_played: 0,
            img_icon_url: None,
            app_type: Some("game".to_string()),
            installdir: None,
            launch: Vec::new(),
            library_art: Default::default(),
        }
    }

    #[test]
    fn cm_catalogue_rejects_invalid_rows_and_tools() {
        let mut tool = protocol_game(4, "Proton runtime");
        tool.app_type = Some("tool".to_string());

        let listed = owned(vec![
            protocol_game(1, "A game"),
            protocol_game(0, "No application id"),
            protocol_game(2, ""),
            protocol_game(3, "   \t"),
            tool,
        ]);

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].app_id, 1);
        assert_eq!(listed[0].name, "A game");
    }

    /// Where a game's pictures are published travels with the game. Without
    /// it, everything released since Steam started addressing artwork by its
    /// contents is a row with no cover, because there is nowhere left to look
    /// for one — see [`crate::art::Published`].
    #[test]
    fn cm_catalogue_carries_where_the_pictures_are() {
        let mut protocol = protocol_game(3288210, "Super Meat Boy 3D");
        protocol.library_art = crate::art::LibraryArt {
            capsule: Some("28dbb244/library_600x900.jpg".to_string()),
            hero: Some("67a1c596/library_hero.jpg".to_string()),
            logo: None,
        };

        let listed = owned(vec![protocol, protocol_game(440, "Team Fortress 2")]);

        assert_eq!(
            listed[0].pictures.of(crate::art::Piece::Cover),
            Some("28dbb244/library_600x900.jpg")
        );
        assert_eq!(
            listed[0].pictures.of(crate::art::Piece::Hero),
            Some("67a1c596/library_hero.jpg")
        );
        assert_eq!(listed[0].pictures.of(crate::art::Piece::Logo), None);
        // And a game whose record says nothing has nothing said about it,
        // rather than a path assembled out of hope.
        assert!(listed[1].pictures.is_empty());
    }

    #[test]
    fn cm_catalogue_clamps_negative_playtime_to_zero() {
        let mut protocol = protocol_game(440, "Team Fortress 2");
        protocol.playtime_forever = -90;

        let listed = owned(vec![protocol]);

        assert_eq!(listed[0].playtime_minutes, 0);
    }

    #[test]
    fn cm_catalogue_preserves_install_and_launch_metadata() {
        let mut protocol = protocol_game(570, "Dota 2");
        protocol.installdir = Some("dota 2 beta".to_string());
        protocol.launch = vec![LaunchEntry {
            executable: "game/bin/linuxsteamrt64/dota2".to_string(),
            arguments: Some("-steam -novid".to_string()),
            workingdir: Some("game/bin/linuxsteamrt64".to_string()),
            launch_type: Some("default".to_string()),
            oslist: Some("linux".to_string()),
            osarch: Some("64".to_string()),
            betakey: Some("public-beta".to_string()),
        }];

        let listed = owned(vec![protocol]);

        assert_eq!(listed[0].install_dir.as_deref(), Some("dota 2 beta"));
        assert_eq!(
            listed[0].launch,
            vec![Launch {
                executable: "game/bin/linuxsteamrt64/dota2".to_string(),
                arguments: Some("-steam -novid".to_string()),
                working_dir: Some("game/bin/linuxsteamrt64".to_string()),
                kind: Some("default".to_string()),
                operating_systems: Some("linux".to_string()),
                architecture: Some("64".to_string()),
                beta_key: Some("public-beta".to_string()),
            }]
        );
    }

    /// The order the column is in: everything installed first, alphabetically,
    /// then everything else, alphabetically.
    #[test]
    fn installed_games_come_first_and_both_halves_are_alphabetical() {
        let listed: Vec<String> = sorted(vec![
            game(4, "Zebra", true),
            game(1, "banana", false),
            game(2, "Apple", true),
            game(3, "cherry", false),
            game(5, "apricot", true),
        ])
        .into_iter()
        .map(|game| game.name)
        .collect();

        assert_eq!(
            listed,
            vec!["Apple", "apricot", "Zebra", "banana", "cherry"],
            "installed first, then the rest, each half by name and not by case"
        );
    }

    /// A game being downloaded cannot be played, so it belongs with the ones
    /// that are not there yet rather than at the top of the column.
    #[test]
    fn a_game_still_downloading_sorts_with_the_uninstalled() {
        let mut downloading = game(2, "Aardvark", true);
        downloading.updating = true;

        let listed: Vec<u32> = sorted(vec![downloading, game(1, "Zebra", true)])
            .into_iter()
            .map(|game| game.app_id)
            .collect();
        assert_eq!(listed, vec![1, 2]);
    }

    /// A library listed by name is listed by name, and does not put the
    /// installed half through the alphabet first: somebody who picked this row
    /// is looking a title up, and half an alphabet twice is the harder of the
    /// two lists to look anything up in.
    #[test]
    fn by_name_is_one_alphabet_and_not_two() {
        let listed = |sort: Sort| {
            sorted_by(
                vec![
                    game(1, "Zebra", true),
                    game(2, "banana", false),
                    game(3, "Apple", false),
                ],
                sort,
            )
            .into_iter()
            .map(|game| game.name)
            .collect::<Vec<_>>()
        };

        assert_eq!(listed(Sort::NameAscending), ["Apple", "banana", "Zebra"]);
        assert_eq!(listed(Sort::NameDescending), ["Zebra", "banana", "Apple"]);
    }

    /// Sizes and playtimes count down from the top, or up from it, and the
    /// name settles two games that stand equal.
    #[test]
    fn the_orders_with_a_direction_run_both_ways() {
        let sized = |app_id: u32, name: &str, size: u64| {
            let mut game = game(app_id, name, true);
            game.size_on_disk = size;
            game
        };
        let played = |app_id: u32, name: &str, minutes: u32| {
            let mut game = game(app_id, name, true);
            game.playtime_minutes = minutes;
            game
        };
        let names = |games: Vec<Game>| {
            games
                .into_iter()
                .map(|game| game.name)
                .collect::<Vec<String>>()
        };

        let sizes = vec![
            sized(1, "Middle", 500),
            sized(2, "Biggest", 900),
            sized(3, "Smallest", 10),
        ];
        assert_eq!(
            names(sorted_by(sizes.clone(), Sort::LargestFirst)),
            ["Biggest", "Middle", "Smallest"]
        );
        assert_eq!(
            names(sorted_by(sizes, Sort::SmallestFirst)),
            ["Smallest", "Middle", "Biggest"]
        );

        // Played for no minutes at all is a quantity rather than a gap: the
        // library asked for least-played-first is being asked what has not been
        // got round to, and the answer belongs at the top.
        let playtimes = vec![
            played(1, "Some", 60),
            played(2, "Most", 6_000),
            played(3, "Never", 0),
        ];
        assert_eq!(
            names(sorted_by(playtimes.clone(), Sort::MostPlayedFirst)),
            ["Most", "Some", "Never"]
        );
        assert_eq!(
            names(sorted_by(playtimes, Sort::LeastPlayedFirst)),
            ["Never", "Some", "Most"]
        );
    }

    /// A game with no size and one that has never been started are last in
    /// both directions, because neither has a number to be at either end of.
    #[test]
    fn what_steam_cannot_say_sorts_last_whichever_way_round_it_is() {
        let mut installed = game(1, "Installed", true);
        installed.size_on_disk = 1_000;
        installed.last_played = 5_000;
        let missing = game(2, "Absent", false);

        for sort in [Sort::LargestFirst, Sort::SmallestFirst] {
            let listed = sorted_by(vec![missing.clone(), installed.clone()], sort);
            assert_eq!(
                listed[0].app_id,
                1,
                "{} put the game with no size first",
                sort.label()
            );
        }
        let listed = sorted_by(
            vec![missing.clone(), installed.clone()],
            Sort::RecentlyPlayedFirst,
        );
        assert_eq!(listed[0].app_id, 1, "a game never played is not the newest");
    }

    /// An order nothing in the library can be sorted by says so, so the menu
    /// can grey the row rather than offer a press that does nothing. A library
    /// with nothing installed knows no sizes, and an account whose last-played
    /// times did not arrive has no dates.
    #[test]
    fn an_order_the_library_cannot_be_put_in_says_so() {
        let bare = vec![game(1, "Owned only", false)];
        let knows = orders(&bare);
        assert_eq!(knows, Orders::default());
        for sort in SORTS {
            assert_eq!(
                sort.orders(knows),
                matches!(
                    sort,
                    Sort::InstalledFirst | Sort::NameAscending | Sort::NameDescending
                ),
                "{}",
                sort.label()
            );
        }

        let mut full = game(1, "Here", true);
        full.size_on_disk = 10;
        full.playtime_minutes = 10;
        full.last_played = 10;
        let knows = orders(&[full]);
        assert!(SORTS.iter().all(|sort| sort.orders(knows)));
    }

    /// The settings file names an order in words the user can read, and every
    /// name reads back as the order it was written for. An order this shell
    /// does not have is nothing rather than an error.
    #[test]
    fn every_order_survives_the_settings_file() {
        for sort in SORTS {
            assert_eq!(Sort::from_key(sort.key()), Some(*sort));
        }
        assert_eq!(Sort::from_key("by vibes"), None);
        assert_eq!(Sort::default(), Sort::InstalledFirst);
    }

    /// Two games with the same name keep the same order between one refresh
    /// and the next, rather than swapping places under the cursor.
    #[test]
    fn a_tie_is_broken_by_something_that_never_changes() {
        let one = sorted(vec![game(7, "Same", false), game(3, "Same", false)]);
        let other = sorted(vec![game(3, "Same", false), game(7, "Same", false)]);
        assert_eq!(one.iter().map(|g| g.app_id).collect::<Vec<_>>(), vec![3, 7]);
        assert_eq!(one, other);
    }

    /// What is on the disk is folded into what is owned, and anything on the
    /// disk that the account does not own is still listed — a family-shared
    /// game is installed and playable, and a column that hid it would
    /// disagree with the machine.
    #[test]
    fn the_disk_and_the_account_are_one_list() {
        let owned = vec![
            game(1, "Owned and installed", false),
            game(2, "Owned only", false),
        ];
        let installed = BTreeMap::from([
            (
                1,
                Installed {
                    app_id: 1,
                    name: "Owned and installed".to_string(),
                    size_on_disk: 1_500_000_000,
                    path: PathBuf::from("/somewhere/common/One"),
                    playable: true,
                    updating: false,
                    downloaded: 0,
                    to_download: 0,
                    tool: false,
                },
            ),
            (
                3,
                Installed {
                    app_id: 3,
                    name: "Installed but not owned".to_string(),
                    size_on_disk: 10,
                    path: PathBuf::from("/somewhere/common/Three"),
                    playable: true,
                    updating: false,
                    downloaded: 0,
                    to_download: 0,
                    tool: false,
                },
            ),
        ]);

        let merged = merge(owned, &installed);
        let by_id = |id: u32| merged.iter().find(|game| game.app_id == id).unwrap();

        assert!(by_id(1).installed);
        assert_eq!(by_id(1).size_on_disk, 1_500_000_000);
        assert_eq!(
            by_id(1).install_path.as_deref(),
            Some(Path::new("/somewhere/common/One"))
        );
        assert!(!by_id(2).installed);
        assert!(by_id(3).installed, "on the disk but not in the account");

        assert_eq!(
            merged.iter().map(|game| game.app_id).collect::<Vec<_>>(),
            vec![3, 1, 2],
            "installed first by name, then the rest"
        );
    }

    /// The line under a row says the one thing that differs between the two
    /// halves of the column.
    #[test]
    fn the_note_under_a_row_says_whether_it_can_be_played() {
        assert_eq!(game(1, "A", false).note(), "Not installed");

        let mut installed = game(1, "A", true);
        installed.size_on_disk = 1_500_000_000;
        assert_eq!(installed.note(), "Installed · 1.5 GB");

        installed.playtime_minutes = 930;
        assert_eq!(installed.note(), "Installed · 1.5 GB · 16 hours played");

        installed.updating = true;
        assert_eq!(installed.note(), "Downloading");
    }

    /// Sizes and playtimes are said the way a person says them.
    #[test]
    fn sizes_and_times_read_as_english() {
        assert_eq!(size_said(0), "0 B");
        assert_eq!(size_said(999), "999 B");
        assert_eq!(size_said(1_000), "1.0 KB");
        assert_eq!(size_said(1_500_000_000), "1.5 GB");
        assert_eq!(size_said(67_960_769_488), "68 GB");

        assert_eq!(time_said(0), "0 min");
        assert_eq!(time_said(59), "59 min");
        assert_eq!(time_said(90), "1.5 hours");
        assert_eq!(time_said(1200), "20 hours");
    }

    /// A manifest read off the disk is the game it describes, including
    /// whether it can be played yet.
    #[test]
    fn a_manifest_says_what_is_installed_and_whether_it_is_ready() {
        let root = std::env::temp_dir().join(format!(
            "lxb-steam-library-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let steamapps = root.join("steamapps");
        std::fs::create_dir_all(steamapps.join("common")).expect("a scratch directory");

        let write = |app_id: u32, flags: u32, owed: u64| {
            let path = steamapps.join(format!("appmanifest_{app_id}.acf"));
            std::fs::write(
                &path,
                format!(
                    r#""AppState" {{
                        "appid" "{app_id}"
                        "name" "Invented {app_id}"
                        "installdir" "Invented {app_id}"
                        "StateFlags" "{flags}"
                        "SizeOnDisk" "4096"
                        "BytesToDownload" "{owed}"
                        "BytesDownloaded" "0"
                    }}"#
                ),
            )
            .expect("writable");
            path
        };

        let ready = read_manifest(&write(999000, 4, 0), &steamapps).expect("a manifest");
        assert_eq!(ready.app_id, 999000);
        assert_eq!(ready.name, "Invented 999000");
        assert_eq!(ready.size_on_disk, 4096);
        assert_eq!(ready.path, steamapps.join("common").join("Invented 999000"));
        assert!(ready.playable);
        assert!(!ready.updating);
        assert!(!ready.tool);

        // Installed *and* carrying an update Steam will apply when it starts.
        // The commonest state a big game sits in, and it is playable: reading
        // `StateFlags` as a state rather than a set is what makes half a
        // library look like it is downloading.
        let patched = read_manifest(&write(999001, 4 | 2, 0), &steamapps).expect("a manifest");
        assert!(
            patched.playable,
            "a game with an update waiting is playable"
        );
        assert!(!patched.updating);

        // Being fetched for the first time: not playable, and listed with the
        // ones that are not there yet.
        let fetching = read_manifest(&write(999002, 1024, 0), &steamapps).expect("a manifest");
        assert!(!fetching.playable);
        assert!(fetching.updating);

        // On the disk, with an update running against it now.
        let busy = read_manifest(&write(999003, 4 | 262144, 0), &steamapps).expect("a manifest");
        assert!(busy.updating, "bytes are moving");

        // Files missing: on the disk, and not something to press until Steam
        // has repaired it.
        let broken = read_manifest(&write(999004, 4 | 32, 0), &steamapps).expect("a manifest");
        assert!(!broken.playable);

        // A compatibility tool declares itself with the file Steam finds it
        // by, and never becomes a row of its own.
        let tool_dir = steamapps.join("common").join("Invented 999005");
        std::fs::create_dir_all(&tool_dir).expect("writable");
        std::fs::write(tool_dir.join("toolmanifest.vdf"), "\"manifest\" {}").expect("writable");
        let tool = read_manifest(&write(999005, 4, 0), &steamapps).expect("a manifest");
        assert!(tool.tool, "Proton and the Linux runtimes are not games");

        // Nor are the redistributables Steam installs beside games.
        let shared = steamapps.join("common").join("Invented 999006");
        std::fs::create_dir_all(shared.join("_CommonRedist")).expect("writable");
        let redist = read_manifest(&write(999006, 4, 0), &steamapps).expect("a manifest");
        assert!(redist.tool);

        let _ = std::fs::remove_dir_all(&root);
    }
}
