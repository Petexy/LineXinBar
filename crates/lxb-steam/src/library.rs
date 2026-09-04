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
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// What is actually happening to it, which is more than those two can say.
    /// See [`Standing`].
    pub standing: Standing,
    /// Whether Steam has an update for this game that it has not finished — a
    /// `TargetBuildID` naming a build other than the `buildid` on the disk.
    ///
    /// The second thing a manifest says about an update, and for a while it is
    /// the *only* thing it says. `StateFlags` does not pick up a working bit
    /// until bytes are moving, and Steam does a good deal before that: it
    /// reconfigures, it preallocates, and — for any update it has interrupted
    /// once already — it verifies the whole of what is on the disk first.
    /// Measured on this machine against Valve's own log: Steam's internal
    /// state read `Update Required,Fully Installed,Update Queued,Update
    /// Running,` and it was reading 10 MB/s off the disk, while the manifest
    /// said `StateFlags 6` and was not written to again for **ten minutes and
    /// thirty-nine seconds**.
    ///
    /// So this is not what the *row* is drawn from — a game with an update
    /// waiting is on the disk and startable, which is the whole reason
    /// [`standing_from`] calls it [`Standing::Ready`]. It is for the one place
    /// that has to know the difference between "nothing is happening" and
    /// "nothing is happening *yet*": a loading screen, which would otherwise
    /// give up sixty seconds into a ten-minute verify and say the game did not
    /// start — and then watch it start.
    ///
    /// **It is three situations and not one**, which is why the two fields
    /// below are here: Steam working silently, Steam having *scheduled* the
    /// work for later, and Steam having tried it and failed. This says only
    /// what the manifest says — that the build on the disk is not the build
    /// Steam means to have there — and whoever asks it has to ask
    /// [`Game::scheduled_for`] and [`Game::last_result`] as well before
    /// calling it work in flight.
    pub update_outstanding: bool,
    /// Whether the manifest says outright that Steam will update this game
    /// before it will start it: `Update Required`, the second bit of
    /// `StateFlags`.
    ///
    /// The bit is the client's own word for the state, and it is not the same
    /// claim as [`Game::update_outstanding`]: that one is inferred from two
    /// build numbers, and there are manifests where the numbers say nothing at
    /// all and this says everything. Read off this machine on 2026-09-03 —
    /// Counter-Strike 2, `StateFlags 6`, `TargetBuildID 0`, `UpdateResult 4`,
    /// `ScheduledAutoUpdate 0`. Every source the shell had was silent: the
    /// builds did not differ, nothing was scheduled, and the last attempt had
    /// ended badly, which the shell reads as work that is *not* in flight. The
    /// row said "Installed", offered Play — and Steam, which will not start a
    /// game in this state until it has fetched the update, opened no window.
    /// The loading screen waited a minute, said the game had not started, and
    /// the shell shut the client down on top of the download Steam had begun,
    /// which left `UpdateResult 4` behind and made Steam put the next attempt
    /// off further. Four presses, and the delay Steam wrote grew from three
    /// hours to three days.
    ///
    /// **Not what the row is drawn from**, for the reason
    /// [`Game::update_outstanding`] is not: a game with an update waiting is on
    /// the disk, is startable, and Steam patches it as it starts it — that is
    /// [`Standing::Ready`] and it is right. What this is for is the one place
    /// that has to know the difference between a window that is about to
    /// appear and a window Steam will not open until it has fetched something:
    /// a loading screen. See `steam::Steam::quietly_working_on_it`.
    pub update_required: bool,
    /// When Steam means to get round to it, as Unix time, or zero for "no
    /// appointment".
    ///
    /// `ScheduledAutoUpdate`. A time in the future is Steam saying it has
    /// looked at this game, decided the update can wait, and put it in the
    /// diary — which is the opposite of the case [`Game::update_outstanding`]
    /// was built for, and reads identically without this. Measured on this
    /// machine: Counter-Strike 2 with `StateFlags 6`, a build two days old and
    /// an appointment for the small hours of the day after tomorrow, while the
    /// client sat idle and would start the game the instant it was asked.
    ///
    /// A time in the *past* is not an appointment: Steam leaves the old one
    /// written after it has kept it, so the comparison needs a clock and is
    /// made where there is one — see `steam::Steam::quietly_updating`.
    pub scheduled_for: u64,
    /// How the last attempt at this game ended: zero for well, anything else
    /// for badly.
    ///
    /// `UpdateResult`. **The enumeration is not known here** and nothing reads
    /// it as one: 4 was seen on a branch switch that failed, and what any other
    /// number means has never been read off a live client. Non-zero is "the
    /// last attempt ended badly, and Steam is not in the middle of another
    /// one", which is all this is asked. Steam clears it to 0 when it picks the
    /// work up again — observed 0 twice mid-update on 2026-09-01 — so a client
    /// that is genuinely working never looks like a failure.
    pub last_result: u64,
    /// Whether the manifest's fully-installed bit is set: there is a whole
    /// copy on the disk, whatever else Steam may be doing on top of it.
    ///
    /// [`Standing`] answers what Steam is *doing*, and for two of its states
    /// that is not enough to know whether there is something underneath.
    /// [`Standing::Validating`] is the one that matters: a check before an
    /// update runs over a complete, playable copy, and a check at the end of a
    /// first download runs over one that has never existed. They are the same
    /// word and they are not the same row — see [`Sort::InstalledFirst`].
    pub fully_installed: bool,
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
            standing: Standing::NotInstalled,
            update_outstanding: false,
            update_required: false,
            scheduled_for: 0,
            last_result: 0,
            fully_installed: false,
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
        game.standing = match installed {
            true => Standing::Ready,
            false => Standing::NotInstalled,
        };
        game.fully_installed = installed;
        game.size_on_disk = u64::from(app_id) * 1_000_000_000;
        game
    }

    /// Say that this invented game is coming down, `share` percent of the way
    /// through — a fixture about the bar under a row rather than about a
    /// library.
    ///
    /// It invents a size as well as a percentage, because the row says both and
    /// a fixture that made up only one of them would be a picture of half of
    /// it. Forty gigabytes: a large game, and large enough that the percentage
    /// is the useful half of the sentence.
    pub fn coming_down(mut self, share: u8) -> Game {
        self.standing = Standing::Downloading;
        self.installed = false;
        self.updating = true;
        // Nothing underneath it, which is what a first download is. See
        // [`Game::fully_installed`] and the order it decides.
        self.fully_installed = false;
        self.to_download = 40_000_000_000;
        self.downloaded = self.to_download / 100 * u64::from(share.min(100));
        self
    }

    /// Say what this invented game is doing, for a fixture that is about a
    /// state rather than about a library.
    ///
    /// Public and not behind `cfg(test)` for the reason [`Game::invented`] is:
    /// the shell's own tests are in another crate, and what they need to build
    /// is a game in a particular state — a copy being updated, one waiting its
    /// turn, one that needs repairing. Building those field by field outside
    /// this module would be four places agreeing about what a standing implies,
    /// which is exactly what [`Standing`] exists to stop.
    pub fn doing(mut self, standing: Standing) -> Game {
        self.standing = standing;
        self.installed = standing.on_the_disk();
        self.updating = standing.moving();
        // What the manifest would have said underneath it. Four of the ten are
        // only ever written over a whole copy — [`standing_from`] will not say
        // `Updating` or `UpdatePaused` without the bit, and `Ready` *is* the
        // bit — and `Validating` is the one that goes either way, so a fixture
        // that wants a check over a first download says so by clearing this
        // itself. See [`Game::fully_installed`].
        self.fully_installed = matches!(
            standing,
            Standing::Ready | Standing::UpdatePaused | Standing::Updating | Standing::Validating
        );
        self
    }

    /// The letter this game is filed under in an index of the library, or
    /// `None` for a name that begins with anything else — a digit, a bracket, a
    /// quotation mark, an alphabet this is not written in.
    ///
    /// Taken from the same folded key the library is *ordered* by, so a game
    /// cannot be filed under one letter and sorted under another: `the witness`
    /// and `The Witness` are one place in the list and one place in the index,
    /// for the same reason and by the same line of code.
    ///
    /// A to Z and nothing else, which is a smaller answer than a name can ask
    /// for and is deliberate. The shell draws a heading as the *letter itself*,
    /// cut out of its own face into the same material its marks are made of, so
    /// the headings are a closed set that is paid for once at startup rather
    /// than a picture made of any string — `icons::INDEX_LETTERS` in the desktop
    /// crate is that set, and this is the half of the agreement that lives with
    /// the library. A name in another script therefore files with the digits and
    /// the brackets, under the one heading that means "not one of these".
    /// What the column is sorted on, for a test that has to check two lists
    /// came out in the same order.
    pub fn order_key(&self) -> &str {
        &self.order
    }

    pub fn initial(&self) -> Option<char> {
        // Already folded, so the first character is the lowercase of the name's
        // first — uppercased here because a heading is a capital, and because
        // the two halves of a name that folds oddly must not become two
        // headings.
        self.order
            .chars()
            .next()
            .filter(char::is_ascii_alphabetic)
            .map(|first| first.to_ascii_uppercase())
    }

    /// Whether this game's name holds what somebody is looking for.
    ///
    /// `needle` is a query already folded by [`sought`], which is the same
    /// folding the name itself went through — so this is one `contains` per
    /// game per keystroke and no allocation at all. A search of a thousand
    /// titles is then cheap enough to answer on the frame the letter was
    /// pressed, which is what a field on this bar has to do.
    ///
    /// A substring rather than a prefix, because the name somebody has in mind
    /// is very often not the one the shop uses: "portal" finds *Portal 2*, and
    /// "witcher" finds *The Witcher 3: Wild Hunt*, which no prefix ever would.
    pub fn matches(&self, needle: &str) -> bool {
        self.order.contains(needle)
    }

    /// How far along the download is, as a share of one, where there is a
    /// download and the disk says enough to tell.
    ///
    /// **The standing decides whether the two numbers mean anything at all** —
    /// see [`Standing::counting_bytes`], and note that a game that is simply
    /// installed is one of the states where they do not. [`Game::note`] prints
    /// its percentage from this same call, so the words on a row and the bar
    /// under them cannot come to different conclusions about whether there is
    /// anything to say.
    pub fn fraction(&self) -> Option<f32> {
        self.standing
            .counting_bytes()
            .then(|| fraction(self.downloaded, self.to_download))
            .flatten()
    }

    /// Whether this manifest is carrying a job that never finished, whatever
    /// its flags say.
    ///
    /// **Reported from use on 2026-09-04, and the manifest is the only trace.**
    /// Street Fighter 6 sat at `StateFlags 4` — Fully Installed, and nothing
    /// else — carrying `BytesDownloaded 32537312` against `BytesToDownload
    /// 1439173440`. Two per cent of a job, on a game the shell called
    /// Installed and started without a word. Steam's own log had been saying
    /// `state changed : Fully Installed,Update Queued,` about it since the
    /// **13th of August**, and the file it wrote one second after that line
    /// carried none of it.
    ///
    /// **The pair is a reliable reading and this is measured, not assumed.**
    /// Over all thirty manifests on that machine, every game whose work had
    /// finished carried the two numbers *exactly equal* — 7238839920 against
    /// 7238839920, 1996126336 against 1996126336, nought against nought — and
    /// the only three that differed all had genuinely unfinished jobs: this
    /// game, and two Protons that said so through their flags as well.
    ///
    /// It reads them for a state [`Standing::counting_bytes`] deliberately
    /// does not, and the two are not in conflict. That answers *how far a
    /// job has got*, which for a game that is simply installed is a bar under
    /// the last thing Steam did to it. This answers *whether there is a job at
    /// all*, and the note against that one — a fully installed Proton carrying
    /// `BytesToDownload 116304` with `BytesDownloaded 0` — is this same
    /// situation seen before there was anything to read it with: on this
    /// machine today, both Protons in that shape carry `Update Required` and
    /// are genuinely part-fetched.
    ///
    /// **What it is used for is chosen so that being wrong is cheap.** Not the
    /// row, which goes on saying Installed, and not whether the game starts —
    /// a false positive there would be the regression that cost two days of a
    /// game. It decides how long Valve's client is left running with nothing
    /// visible to do, where the cost of a false positive is an invisible
    /// program staying up a minute or two longer.
    pub fn work_outstanding(&self) -> u64 {
        self.to_download.saturating_sub(self.downloaded)
    }

    /// What goes under the name on the bar.
    ///
    /// The one line the row has to say something in, so it says the thing that
    /// differs between the two halves of the column: whether this is a game
    /// that can be started right now.
    pub fn note(&self) -> String {
        // Everything that is not simply here or simply absent says what it is,
        // in its own words, with the percentage where there is one — and
        // [`Game::fraction`] is the whole of "where there is one", so the line
        // and the bar under it are answering from the same place. It is not
        // only the size being unknown: on most of the standings the manifest's
        // two byte counts are the *last* operation's, and a game being checked
        // used to read "Checking files · 100% of 626 MB" off numbers that had
        // nothing to do with the check.
        if !matches!(self.standing, Standing::NotInstalled | Standing::Ready) {
            return self.how_far(self.standing.said());
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

impl Game {
    /// The same line, for work no Steam is doing.
    ///
    /// The manifest says what Valve's client was in the middle of when it was
    /// last running, and says it for as long as nobody starts Steam again.
    /// Whether anything is *actually* fetching this is a question about the
    /// machine rather than about the file — see [`crate::Steam::client_is_running`]
    /// — and where the answer is no, this is what the row says instead of
    /// "Updating".
    ///
    /// How far it had got is kept, because it is the one useful thing the
    /// manifest still knows: a 1.3 GB update that stopped at a third is a
    /// different thing to come back to from one that never started.
    pub fn note_waiting_for_steam(&self) -> String {
        self.how_far(WAITING_FOR_STEAM)
    }

    /// The same line, for work Steam has begun without saying so in the
    /// manifest.
    ///
    /// A word and no percentage, and there is no percentage to be had: nothing
    /// on the disk is counting this, and the manifest's two byte counts belong
    /// to whatever Steam did to this game last and have nothing to do with what
    /// it is doing now.
    ///
    /// **The word is given rather than assumed**, because two different things
    /// arrive here and calling both of them an update was the mistake
    /// [`standing_from`] already refuses to make: a check on a large game runs
    /// for minutes and is not a download. Where it *is* an update, the word is
    /// the one the row will use once the same update starts counting, because
    /// it is the same update — a loading screen that said one thing for the ten
    /// minutes of verifying and another for the twenty seconds of downloading
    /// would be describing two events where the user is waiting through one.
    pub fn note_working(&self, standing: Standing) -> String {
        standing.said().to_string()
    }

    /// One sentence and the percentage under it, where there is one.
    ///
    /// Shared by both notes so that a row cannot say how far along it is in one
    /// state and not in another — and [`Game::fraction`] is the whole of "where
    /// there is one", so the line and the bar under it answer from the same
    /// place.
    fn how_far(&self, said: &str) -> String {
        match self.fraction() {
            None => said.to_string(),
            Some(share) => format!(
                "{said} · {:.0}% of {}",
                share * 100.0,
                size_said(self.to_download)
            ),
        }
    }
}

/// What a row says about work Valve's client has begun and is not doing.
///
/// One sentence for every one of those states rather than a word each, because
/// the difference between a stopped update and a stopped download is not what
/// the person in front of it has to know: what they have to know is that
/// nothing is happening and that starting Steam is what would change it.
pub const WAITING_FOR_STEAM: &str = "Waiting for Steam";

/// How far `done` is through `total`, or nothing where there is not yet an
/// answer.
///
/// One function because the shell asks this of two different sources — the
/// manifest on the disk, and the running count of an install this session
/// started — and a bar that read one of them differently from the other would
/// move differently depending on who began the download.
///
/// **Nothing, rather than nought, in two cases.** No total is the client
/// writing the manifest before it knows how large the download is. No bytes is
/// a download that has been asked for and has not begun — which is a state the
/// shell already has a word for, `Waiting to download`, and "0%" is not a
/// reading but the absence of one. A row that printed it, and a groove drawn
/// empty beside it, is what a person saw for the whole of a short install.
///
/// Clamped, because the two numbers come from a file another program is
/// writing: a manifest caught between a new `BytesDownloaded` and a new
/// `BytesToDownload` can say more has arrived than was asked for, and a bar
/// drawn past the end of its own groove is a bug the user sees.
pub fn fraction(done: u64, total: u64) -> Option<f32> {
    (total > 0 && done > 0).then(|| (done as f64 / total as f64).clamp(0.0, 1.0) as f32)
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
    /// Everything with a copy on the disk that runs, then everything else,
    /// each half by name. The one the library is kept in, and so the only one
    /// that costs nothing — see [`sorted`].
    ///
    /// "That runs" rather than "that can be started this second": a game Steam
    /// is updating or checking has a whole copy underneath and keeps its place
    /// while that happens, because a row moving down the column is the one
    /// thing an update must not do to the game somebody was looking at. See
    /// [`Sort::compare`], where that is written out.
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
            //
            // **A game being *updated* is not that**, and this used to treat
            // them alike. There is a playable copy on the disk underneath an
            // update — that is the whole difference between `Updating` and
            // `Downloading`, and [`standing_from`] only says `Updating` where
            // the fully-installed bit is set — so a game keeps its place while
            // Steam works on it. Reported by the user, who watched a row they
            // had just come back to jump down the column the moment Steam wrote
            // its first working bit: *it is not just cover as it moves the
            // entry*. Somewhere else on the bar is not where the game they
            // were looking at should be.
            //
            // So the question is **whether there is a copy that runs**, now or
            // underneath, rather than whether the row is busy — and the first
            // cut of it asked neither. `installed && !updating` is
            // [`Standing::on_the_disk`] and [`Standing::moving`], and two of
            // the ten states are on the disk and still: a paused download with
            // nothing playable in it, and a copy with files missing. Both
            // sorted with the games that can be played, which is where nobody
            // will look for them.
            //
            // [`Standing::Validating`] is the other half of the row movement
            // the user reported, and it is the half a standing cannot answer
            // on its own. Steam full-verifies before any update it has been
            // interrupted in once, so the ordinary shape of an update is
            // `Validating` and then `Updating` — and a row that counted only
            // the second dropped down the column, sat there for the length of
            // a verify, and came back. Whether there is anything underneath is
            // what tells that from the check at the end of a first download,
            // and it is the manifest's own bit rather than an inference: see
            // [`Game::fully_installed`].
            Sort::InstalledFirst => {
                let ready = |game: &Game| {
                    game.standing.playable()
                        || (game.fully_installed
                            && matches!(game.standing, Standing::Updating | Standing::Validating))
                };
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

/// What a typed query becomes before it is compared with any name, or `None`
/// for a query that asks nothing.
///
/// Folded exactly as a name is — see [`sort_key`] — because the two are about
/// to be compared, and a search that folded them differently would be a field
/// that finds nothing for anybody typing in the case they see on screen. Done
/// once for the whole library rather than once per game.
///
/// The empty answer is the whole of "not searching": a query of spaces is a
/// query of nothing, and it must show the library rather than the games whose
/// names contain a space.
pub fn sought(query: &str) -> Option<String> {
    let folded = sort_key(query);
    (!folded.is_empty()).then_some(folded)
}

/// Turn the CM/PICS catalogue into this crate's deliberately smaller model.
/// One title as [`crate::catalogue`] remembers it: the account's half of a
/// [`Game`] and nothing about this machine's disk.
pub struct Remembered {
    pub app_id: u32,
    pub name: String,
    pub playtime_minutes: u32,
    pub last_played: u32,
    pub launch: Vec<Launch>,
    pub pictures: crate::art::Published,
}

/// Build the owned library out of remembered records.
///
/// The same [`sorted`] a live answer goes through, so a column restored from
/// the disk and one that has just arrived from Steam are the same column.
pub fn owned_from_records(games: impl Iterator<Item = Remembered>) -> Vec<Game> {
    sorted(
        games
            .map(|remembered| {
                let mut game = Game::new(
                    remembered.app_id,
                    remembered.name,
                    remembered.playtime_minutes,
                );
                game.last_played = remembered.last_played;
                game.launch = remembered.launch;
                game.pictures = remembered.pictures;
                game
            })
            .collect(),
    )
}

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
            // Where the pictures are and what the icon's hash is arrive in
            // two different parts of one PICS record, so they are put together
            // here rather than converted from the first alone.
            owned.pictures =
                crate::art::Published::from_pics(game.library_art, game.img_icon_url.as_deref());
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

/// Everything one Steam has installed, by app id.
///
/// Read from the files that client keeps, so this is what Steam itself believes
/// rather than a second record of the same thing — which is exactly why it has
/// to be *that* client's files. See [`crate::backend`].
pub fn installed_for(backend: &crate::backend::Backend) -> BTreeMap<u32, Installed> {
    installed_in(&backend.libraries())
}

/// Everything installed across these libraries, by app id.
pub fn installed_in(libraries: &[PathBuf]) -> BTreeMap<u32, Installed> {
    let mut found = BTreeMap::new();
    for library in libraries {
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
    /// What Steam is doing with it, or has stopped doing.
    pub standing: Standing,
    /// How far that has got, in bytes, when it is under way. Both zero for a
    /// game that is not being fetched — and, briefly, for one that is: the
    /// client writes the manifest before it knows the size.
    pub downloaded: u64,
    pub to_download: u64,
    /// Whether Steam means to put a different build on the disk than the one
    /// that is on it. See [`Game::update_outstanding`], which is the same
    /// question asked where the shell asks it.
    pub update_outstanding: bool,
    /// And whether the manifest says outright that Steam will update this game
    /// before it will start it. See [`Game::update_required`].
    pub update_required: bool,
    /// When Steam means to do it, as Unix time, or zero. See
    /// [`Game::scheduled_for`].
    pub scheduled_for: u64,
    /// How the last attempt at it ended, or zero for well. See
    /// [`Game::last_result`].
    pub last_result: u64,
    /// Whether there is a whole copy on the disk under whatever Steam is
    /// doing. See [`Game::fully_installed`].
    pub fully_installed: bool,
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
/// The bits Steam writes into `StateFlags`.
///
/// Every one named here was **read off a real client** rather than taken from a
/// table on the internet, and two of the ones on those tables are wrong for this
/// client. `content_log.txt` prints each change twice over — once in words
/// (`AppID 945360 state changed : Update Required,Update Queued,Update Started,`)
/// and once as a number
/// (`scheduler finished : … state 0x40a`) — so the pairs decode each other.
/// `0x40a` is 2|8|1024 against those three words, `0xc` is 4|8 against
/// `Fully Installed,Update Queued`, and `0x200c` is 4|8|8192 against
/// `Fully Installed,Update Queued,App Running`.
///
/// So **8 is `Update Queued`** (the tables call it Encrypted), **16 is
/// `Update Optional`** (the tables call it Locked) and **8192 is `App
/// Running`**. Reading this off the machine is what found the bug below.
mod state {
    pub const FULLY_INSTALLED: u64 = 4;
    /// Steam will not start this game until it has updated it. Read off this
    /// machine on 2026-09-03, where it is the *only* thing that said so: see
    /// [`Installed::update_required`].
    pub const UPDATE_REQUIRED: u64 = 2;
    /// In Steam's download queue and not started yet. See the note above: the
    /// published tables call this bit something else entirely.
    pub const QUEUED: u64 = 8;
    /// Files gone or damaged: on the disk, and not playable until Steam has
    /// repaired it.
    pub const BROKEN: u64 = 32 | 128;
    pub const UNINSTALLING: u64 = 2048;
    /// Begun and stopped, with everything that had arrived still on the disk.
    /// Steam is waiting to be told to go on.
    pub const PAUSED: u64 = 512;
    /// Checking what is on the disk against what should be there. It is not a
    /// download and saying so is the point: a validate on a 70 GB game is
    /// twenty minutes of a row that must not read "Downloading".
    pub const VALIDATING: u64 = 131072;
    /// Every bit that means bytes are moving: an update started, running,
    /// validating, reconfiguring, adding files, preallocating, downloading,
    /// staging or committing.
    pub const WORKING: u64 =
        1024 | 256 | 16384 | 32768 | 65536 | 131072 | 262144 | 524288 | 1048576;
}

/// What one title is actually doing.
///
/// Two booleans used to carry this — `playable` and `updating` — and between
/// them they could not say most of what a game can be. A paused download and a
/// download that has not started yet were the same thing; so were a game being
/// validated and a game being fetched; and a copy with files missing came out
/// as *not installed*, which is a row offering to fetch a game that is already
/// there and needs repairing instead.
///
/// The order is the order a game moves through, which is what makes the
/// comparisons below readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    /// Not on this disk. The account may own it; nothing here has it.
    NotInstalled,
    /// Steam has been asked for it and nothing has begun to arrive. The manifest
    /// exists and says nothing has been written.
    Queued,
    /// Bytes are moving, and there has never been a playable copy.
    Downloading,
    /// Bytes are moving over a copy that is already playable. It is the same
    /// work and a different sentence: nothing is missing, and what is on the
    /// disk would still run.
    Updating,
    /// Steam is checking what is on the disk against what should be there.
    Validating,
    /// Begun and stopped, with what arrived still on the disk and nothing
    /// playable in it.
    Paused,
    /// On the disk and playable, with an update to it begun and stopped.
    ///
    /// The same work as [`Standing::Paused`] and a different sentence, exactly
    /// as [`Standing::Updating`] is to [`Standing::Downloading`]: nothing is
    /// missing and what is there would still run. It exists because a real
    /// machine had one and the shell would not start it — a game somebody had
    /// been playing that afternoon, sitting under "Download paused · 100%",
    /// with no Play on its row.
    UpdatePaused,
    /// On the disk, not moving, and not playable: files missing or corrupt.
    Broken,
    /// Being taken off the disk.
    Uninstalling,
    /// On the disk and playable.
    Ready,
}

impl Standing {
    /// Whether it can be started right now.
    pub fn playable(self) -> bool {
        matches!(self, Standing::Ready | Standing::UpdatePaused)
    }

    /// Whether it can be started with no connection to Steam.
    ///
    /// Narrower than [`Standing::playable`] by exactly one state, and the state
    /// is Valve's rule rather than this shell's: "Only games that are fully
    /// up-to-date will be available" is what its own Offline Mode dialog
    /// promises, and a client in that mode answers a game with an update
    /// waiting with "This game is not ready to be played in Offline Mode."
    ///
    /// `UpdatePaused` is the whole state that falls away — a complete copy on
    /// the disk, played yesterday, with an update Steam has stopped. Online it
    /// starts perfectly, which is why it is playable at all.
    pub fn playable_offline(self) -> bool {
        self == Standing::Ready
    }

    /// Whether there is a copy on this disk at all, however unusable.
    ///
    /// The difference between "fetch this" and "repair this", which is the one
    /// the old pair of booleans could not draw: a broken copy read as absent,
    /// so the row offered a download for a game already taking up the space.
    pub fn on_the_disk(self) -> bool {
        self != Standing::NotInstalled
    }

    /// Whether Steam is doing something to it that will finish on its own.
    pub fn moving(self) -> bool {
        matches!(
            self,
            Standing::Queued
                | Standing::Downloading
                | Standing::Updating
                | Standing::Validating
                | Standing::Uninstalling
        )
    }

    /// Whether bytes are arriving for it *now*.
    ///
    /// Narrower than [`Self::moving`] and deliberately so, because what this
    /// answers is "is there a download to put on the screen": a game waiting
    /// its turn behind another has nothing arriving and nothing to count, one
    /// being checked over is not downloading, and one being removed is the
    /// opposite. Steam runs one download at a time, so at most one game in a
    /// library answers yes.
    pub fn arriving(self) -> bool {
        matches!(self, Standing::Downloading | Standing::Updating)
    }

    /// Whether there is a download here to stop — one running, or one stopped
    /// part way with bytes on the disk.
    /// [`Standing::Updating`] and [`Standing::UpdatePaused`] are deliberately
    /// not here, and it is the same reason for both: what this offers is
    /// stopping *and deleting what arrived*, and under an update there is a
    /// whole playable copy that nobody asked to have deleted.
    pub fn is_a_download(self) -> bool {
        matches!(
            self,
            Standing::Queued | Standing::Downloading | Standing::Paused
        )
    }

    /// Whether it has stopped without finishing and without failing, and so is
    /// waiting for somebody rather than for Steam.
    ///
    /// The state a watch has to end on. A row that counted a download used to
    /// go on counting it for the rest of the session when Steam paused it or
    /// the copy turned out to need repairing, because neither is an arrival and
    /// neither is a failure and nothing else ever said so.
    pub fn waiting_for_somebody(self) -> bool {
        matches!(
            self,
            Standing::Paused | Standing::UpdatePaused | Standing::Broken
        )
    }

    /// Whether `BytesDownloaded` and `BytesToDownload` are about *this* game
    /// right now.
    ///
    /// Five of the ten, and on the other five the two numbers are leftovers from
    /// whatever Steam last did with the app. That is not a theory: on the
    /// machine this was written against, a fully installed Proton carries
    /// `BytesToDownload 116304` with `BytesDownloaded 0`, and a fully installed
    /// Steam runtime beside it carries the two equal. Read without this, the
    /// first is a bar sitting at nought under a game that is entirely here and
    /// the second is a full one.
    ///
    /// `Validating`, `Broken` and `Uninstalling` are left out for the same
    /// reason rather than a different one: Steam is doing something to the game
    /// that is not measured in downloaded bytes, so the pair are the previous
    /// download's and mean nothing about the work in front of the user.
    ///
    /// `Queued` **is** here, which is not the same question as whether there is
    /// anything to show: a download waiting behind another one has a real size
    /// and may already have part of itself on the disk. Whether *anything has
    /// arrived* is [`fraction`]'s to answer, and it answers it once for both
    /// sources.
    pub fn counting_bytes(self) -> bool {
        matches!(
            self,
            Standing::Queued
                | Standing::Downloading
                | Standing::Updating
                | Standing::Paused
                | Standing::UpdatePaused
        )
    }

    /// The word for it, on its own row.
    pub fn said(self) -> &'static str {
        match self {
            Standing::NotInstalled => "Not installed",
            Standing::Queued => "Waiting to download",
            Standing::Downloading => "Downloading",
            Standing::Updating => "Updating",
            Standing::Validating => "Checking files",
            Standing::Paused => "Download paused",
            Standing::UpdatePaused => "Update paused",
            Standing::Broken => "Needs repairing",
            Standing::Uninstalling => "Removing",
            Standing::Ready => "Installed",
        }
    }
}

fn read_manifest(path: &Path, steamapps: &Path) -> Option<Installed> {
    let node = vdf::parse(&std::fs::read_to_string(path).ok()?);
    let app_id = u32::try_from(node.number(&["AppState", "appid"])?).ok()?;
    let install_dir = node.string(&["AppState", "installdir"])?;
    let flags = node.number(&["AppState", "StateFlags"]).unwrap_or_default();
    let where_it_is = steamapps.join("common").join(install_dir);

    let downloaded = node
        .number(&["AppState", "BytesDownloaded"])
        .unwrap_or_default();

    // Which build is here, and which one Steam means to have here. Both are
    // written long before `StateFlags` admits to anything — see
    // [`Game::update_outstanding`] — and `TargetBuildID` is zero for the
    // moment between Steam picking the app up and knowing what it wants, which
    // is not a difference and must not read as one.
    let build = node.number(&["AppState", "buildid"]).unwrap_or_default();
    let wanted = node
        .number(&["AppState", "TargetBuildID"])
        .unwrap_or_default();

    // And the two fields that say Steam is *not* getting on with it, which are
    // written in the same silence. A build that differs is true of a client
    // working quietly, of one that has put the work off until Tuesday, and of
    // one that tried and failed, and the three read identically without these.
    // Absent in both directions is the harmless answer: an older manifest with
    // neither key says nothing is scheduled and nothing went wrong, which is
    // what a manifest that has never been updated is.
    let scheduled_for = node
        .number(&["AppState", "ScheduledAutoUpdate"])
        .unwrap_or_default();
    let last_result = node
        .number(&["AppState", "UpdateResult"])
        .unwrap_or_default();

    Some(Installed {
        app_id,
        name: node
            .string(&["AppState", "name"])
            .unwrap_or(install_dir)
            .to_string(),
        size_on_disk: node.number(&["AppState", "SizeOnDisk"]).unwrap_or_default(),
        downloaded,
        to_download: node
            .number(&["AppState", "BytesToDownload"])
            .unwrap_or_default(),
        standing: standing_from(flags, downloaded),
        update_outstanding: wanted != 0 && wanted != build,
        update_required: flags & state::UPDATE_REQUIRED != 0,
        scheduled_for,
        last_result,
        fully_installed: flags & state::FULLY_INSTALLED != 0,
        tool: is_tool(&where_it_is),
        path: where_it_is,
    })
}

/// What one manifest's `StateFlags` mean, in order of what wins.
///
/// The order is the whole of it, and every line of it is a case the two
/// booleans got wrong. Removal outranks everything, because a game being taken
/// off the disk is not a game that is installed however installed it still
/// looks. Repair outranks work, because a copy with files missing has to say so
/// even while Steam is fetching the missing ones — otherwise "Downloading" is
/// the answer to "why will this not start", for twenty minutes. Validating
/// outranks downloading because it is not a download: a check on a 70 GB game
/// is a long time to sit under a percentage that never moves.
///
/// `downloaded` is what tells a queued title from one that is arriving. Steam
/// writes the manifest when it takes the request and before a byte has landed,
/// and a row reading "Downloading 0%" for the first seconds of every install
/// reads as a stall.
fn standing_from(flags: u64, downloaded: u64) -> Standing {
    if flags & state::UNINSTALLING != 0 {
        return Standing::Uninstalling;
    }
    let installed = flags & state::FULLY_INSTALLED != 0;
    if flags & state::BROKEN != 0 {
        return Standing::Broken;
    }
    if flags & state::VALIDATING != 0 {
        return Standing::Validating;
    }
    // Before the working bits, and that is the point of it being here at all:
    // Steam leaves `UpdateStarted` set on a download it has paused, so a
    // paused install with bytes on the disk reads as one that is arriving
    // unless this is asked first — a row counting a percentage that will not
    // move again until somebody says so.
    //
    // And whether there is a playable copy under it decides which of the two
    // pauses it is. Read off a real machine: a game that had been played that
    // afternoon carried `StateFlags` 516 — installed, with a paused update —
    // and the shell called the whole row a paused download and offered no way
    // to start it.
    if flags & state::PAUSED != 0 {
        return match installed {
            true => Standing::UpdatePaused,
            false => Standing::Paused,
        };
    }
    if flags & state::WORKING != 0 {
        return match (installed, downloaded) {
            (true, _) => Standing::Updating,
            (false, 0) => Standing::Queued,
            (false, _) => Standing::Downloading,
        };
    }
    if installed {
        return Standing::Ready;
    }
    // A manifest, no work in flight, and not installed. Two different things
    // land here and this used to call both of them a pause — which is what a
    // person saw for the first seconds of **every** install they started.
    //
    // Read off the client's own log, a fresh install begins
    // `Update Required,` (2), then `Update Required,Update Queued,` (10), and
    // only then picks up a working bit. Neither of those first two is a pause:
    // nothing has stopped, because nothing has started. Steam says so itself
    // with `Update Queued`, and where even that is not set yet, an empty disk
    // says it — a download that stopped part way has what arrived still on it.
    //
    // The cost of getting this wrong was not only the word. `Paused` is
    // `waiting_for_somebody`, so [`crate::Watching::report`] ended the watch on
    // it: the shell let go of the install it had just been asked to make,
    // two seconds after being asked, and every number on that row after that
    // came from the disk alone.
    if flags & state::QUEUED != 0 || downloaded == 0 {
        return Standing::Queued;
    }
    Standing::Paused
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
        game.standing = on_disk.standing;
        // The two booleans the column has always been drawn from, kept as what
        // they now are: two questions about the standing. A row that says
        // "installed" means the game is here in some form, which is what the
        // ordering and the colour of the cover are about; whether it can be
        // *started* is `Standing::playable`, and the press asks that.
        game.installed = on_disk.standing.on_the_disk();
        game.updating = on_disk.standing.moving();
        game.update_outstanding = on_disk.update_outstanding;
        game.update_required = on_disk.update_required;
        game.scheduled_for = on_disk.scheduled_for;
        game.last_result = on_disk.last_result;
        game.fully_installed = on_disk.fully_installed;
        game.size_on_disk = on_disk.size_on_disk;
        game.install_path = Some(on_disk.path.clone());
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

/// Where one Steam keeps its libraries.
///
/// The first library is wherever that Steam is installed; the rest are listed
/// in its own `libraryfolders.vdf`, which is how a second disk or an external
/// drive gets one. Every path is checked as it is read: a library on a drive
/// that is not plugged in today is simply not one of them.
///
/// Asked of a root rather than of the machine, because the machine can have two
/// Steams and answering for the wrong one is invisible — see
/// [`crate::backend`], which is what decides which root this is.
pub fn libraries_below(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut add = |path: PathBuf| {
        if path.join("steamapps").is_dir() && !found.contains(&path) {
            found.push(path);
        }
    };

    if !looks_like_a_root(root) {
        return found;
    }
    add(root.to_path_buf());

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

/// One Steam library and how much room is left where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub path: PathBuf,
    /// Bytes free to this user on the filesystem holding it, or `None` where
    /// the filesystem would not say — a drive that has been unplugged since the
    /// library list was read, or one this user may not ask about.
    pub free: Option<u64>,
}

impl Room {
    /// How much room is left, as a row says it, or nothing where the answer is
    /// not known.
    ///
    /// Not knowing is said as not knowing. A preflight that guessed at free
    /// space would be a preflight that told somebody a forty-gigabyte download
    /// would fit on a disk that has since been unplugged.
    pub fn said(&self) -> Option<String> {
        Some(format!("{} free", size_said(self.free?)))
    }
}

/// Every Steam library on this machine, with the room left on each.
///
/// In Steam's own order, which is the order it lists them in and begins with
/// the one it is installed in. Which of them a download actually lands in is
/// Steam's decision and this does not pretend otherwise — see
/// [`crate::webui::install`], which deliberately does not choose the folder.
/// What this is for is the question somebody actually has before agreeing to a
/// download, which is whether there is room for it anywhere.
pub fn room_in_each(backend: &crate::backend::Backend) -> Vec<Room> {
    backend
        .libraries()
        .into_iter()
        .map(|path| Room {
            free: free_space(&path),
            path,
        })
        .collect()
}

/// Bytes free to this user on the filesystem holding `path`.
///
/// `f_bavail` rather than `f_bfree`: the difference is the reserve the
/// filesystem keeps for root, which nothing Steam does can spend. Reporting
/// space a download cannot actually use is how a preflight comes to say a game
/// will fit and then have it stop half way with the disk full.
fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: a zeroed `statvfs` is a valid buffer for the call to fill, and
    // the path is a NUL-terminated C string that lives across it.
    let stats = unsafe {
        let mut stats: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(raw.as_ptr(), &mut stats) == 0).then_some(stats)?
    };
    // `f_frsize` is the fragment size the counts are in; `f_bsize` is what a
    // read is done in and is not the same number on every filesystem.
    //
    // Widened rather than converted: both are unsigned and neither is wider
    // than this on any target, but they are not the *same* width everywhere —
    // `f_frsize` is a `c_ulong`, which is half as wide on a 32-bit machine.
    (stats.f_bavail as u64).checked_mul(stats.f_frsize as u64)
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

/// A size, as a row says it, for whoever outside this crate has one to say.
pub fn said(bytes: u64) -> String {
    size_said(bytes)
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

    /// A manifest whose flags say nothing can still be carrying an unfinished
    /// job, and the two byte counts are the only place it is written down.
    ///
    /// Every manifest here is off this machine on 2026-09-04, verbatim. The
    /// first is the one that was reported — a game the shell called Installed
    /// and started without a word, over two per cent of a 1.4 GB job Steam had
    /// been carrying since the 13th of August.
    #[test]
    fn a_manifest_can_carry_a_job_none_of_its_flags_admit_to() {
        let outstanding = |downloaded, to_download| {
            let mut game = game(1, "one", true);
            game.downloaded = downloaded;
            game.to_download = to_download;
            game.work_outstanding()
        };

        // Street Fighter 6, `StateFlags 4`: fully installed, nothing else set.
        assert_eq!(
            outstanding(32_537_312, 1_439_173_440),
            1_406_636_128,
            "two per cent of a job, under a manifest that admits to none"
        );
        // The two Protons, which say so through their flags as well.
        assert_eq!(outstanding(0, 174_464), 174_464);
        assert_eq!(outstanding(0, 38_370_608), 38_370_608);

        // And every game on that disk whose work had finished. The pair is
        // exactly equal on all of them, which is what makes the reading above
        // worth anything at all.
        for (downloaded, to_download) in [
            (7_238_839_920, 7_238_839_920),
            (1_996_126_336, 1_996_126_336),
            (102_773_008, 102_773_008),
            (176_015_840, 176_015_840),
            (0, 0),
        ] {
            assert_eq!(
                outstanding(downloaded, to_download),
                0,
                "a finished job leaves the two equal"
            );
        }

        // A manifest caught mid-write can say more has arrived than was asked
        // for. That is not work outstanding and must not read as a number.
        assert_eq!(outstanding(500, 400), 0);
    }

    fn game(app_id: u32, name: &str, installed: bool) -> Game {
        let mut game = Game::new(app_id, name.to_string(), 0);
        game.installed = installed;
        // And the standing that goes with it, because `installed` is one
        // question about the standing rather than a fact beside it. A fixture
        // that set the boolean alone was a game the column would file in one
        // half and every other reader would call not installed.
        game.standing = match installed {
            true => Standing::Ready,
            false => Standing::NotInstalled,
        };
        game.fully_installed = installed;
        game
    }

    /// The line for work no Steam is doing keeps how far it had got.
    ///
    /// The manifest's two byte counts are the one useful thing it still knows
    /// about a stopped update: a 1.3 GB one that reached a third is a different
    /// thing to come back to from one that never started. Read through the same
    /// builder the ordinary note uses, so a row cannot say how far along it is
    /// in one state and not in another.
    #[test]
    fn a_stopped_update_still_says_how_far_it_got() {
        let mut game = game(1, "A Game", true).doing(Standing::Updating);
        game.to_download = 1_300_000_000;
        game.downloaded = game.to_download / 3;
        assert_eq!(game.note(), "Updating · 33% of 1.3 GB");
        assert_eq!(
            game.note_waiting_for_steam(),
            "Waiting for Steam · 33% of 1.3 GB"
        );

        // And where the manifest has not said yet — the client writes it when
        // it takes the request, before it knows the size — the sentence stands
        // on its own rather than reading nought.
        let fresh = game.clone();
        let mut fresh = fresh;
        fresh.downloaded = 0;
        fresh.to_download = 0;
        assert_eq!(fresh.note_waiting_for_steam(), "Waiting for Steam");
    }

    /// What an index of the library files a name under, which is the same
    /// letter the list is ordered by and not a second reading of the name.
    #[test]
    fn a_game_is_filed_under_the_letter_it_sorts_under() {
        let filed = |name: &str| game(1, name, false).initial();

        assert_eq!(filed("Celeste"), Some('C'));
        assert_eq!(filed("celeste"), Some('C'), "one name is one place");
        assert_eq!(filed("  The Witness"), Some('T'), "as the order trims it");
        // A to Z is the whole of the set, because a heading is drawn as the
        // letter and the shell cuts those from its own face. Everything else
        // files under the one heading that is not a letter.
        assert_eq!(filed("Портал"), None);
        assert_eq!(filed("Łowca"), None);
        assert_eq!(filed("112 Operator"), None);
        assert_eq!(filed("[redacted]"), None);
        assert_eq!(filed(""), None);
    }

    /// What a field at the head of the library keeps, which is decided by the
    /// same folding the order is: somebody types what they can see, and what
    /// they can see is the name in the case the shop wrote it in.
    #[test]
    fn a_name_is_looked_for_the_way_it_is_ordered() {
        let found = |name: &str, query: &str| match sought(query) {
            Some(needle) => game(1, name, false).matches(&needle),
            // Nothing asked keeps everything, which is what the shell shows
            // for a field nobody has typed into.
            None => true,
        };

        assert!(found("Portal 2", "portal"));
        assert!(found("Portal 2", "PORTAL"), "case is not the question");
        assert!(found("Portal 2", "  portal "), "nor the spaces round it");
        // Anywhere in the name, because the name somebody has in mind is very
        // often not the one the shop begins with.
        assert!(found("The Witcher 3: Wild Hunt", "witcher"));
        assert!(found("The Witcher 3: Wild Hunt", "wild hunt"));
        assert!(!found("Portal 2", "celeste"));

        // A query of nothing is not a search, however many spaces it is made
        // of — a field with a space in it must not answer with every name that
        // has one.
        assert_eq!(sought(""), None);
        assert_eq!(sought("   "), None);
        assert!(found("Portal 2", "   "));
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
        let downloading = game(2, "Aardvark", true).doing(Standing::Downloading);

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
                    standing: Standing::Ready,
                    downloaded: 0,
                    to_download: 0,
                    update_outstanding: false,
                    update_required: false,
                    scheduled_for: 0,
                    last_result: 0,
                    fully_installed: true,
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
                    standing: Standing::Ready,
                    downloaded: 0,
                    to_download: 0,
                    update_outstanding: false,
                    update_required: false,
                    scheduled_for: 0,
                    last_result: 0,
                    fully_installed: true,
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

        // And every state that is neither simply here nor simply absent says
        // which one it is, in its own words, with the percentage where there is
        // one. Two booleans could say none of this: a paused download, a check
        // running over a 70 GB game and a copy with files missing were all
        // "Downloading" or "Not installed".
        let counting = |standing: Standing| {
            let mut game = game(1, "A", true).doing(standing);
            game.downloaded = 3_000_000_000;
            game.to_download = 6_000_000_000;
            game.note()
        };
        assert_eq!(
            counting(Standing::Downloading),
            "Downloading · 50% of 6.0 GB"
        );
        assert_eq!(counting(Standing::Updating), "Updating · 50% of 6.0 GB");
        assert_eq!(
            counting(Standing::Paused),
            "Download paused · 50% of 6.0 GB"
        );
        assert_eq!(
            counting(Standing::UpdatePaused),
            "Update paused · 50% of 6.0 GB"
        );

        // And the states where those same two numbers are the *last*
        // operation's say their word and no number at all. A check is not
        // measured in downloaded bytes; nor is a repair, nor a removal. Each of
        // these used to print a percentage off whatever the previous download
        // had left in the manifest — which on this machine is a fully installed
        // Proton carrying `BytesToDownload 116304` and `BytesDownloaded 0`.
        assert_eq!(counting(Standing::Validating), "Checking files");
        assert_eq!(counting(Standing::Broken), "Needs repairing");
        assert_eq!(counting(Standing::Uninstalling), "Removing");

        // No size yet: the client writes the manifest before it knows one, and
        // a row reading "Downloading 0%" for the first seconds of an install
        // reads as a stall.
        let mut queued = game(1, "A", true).doing(Standing::Queued);
        queued.downloaded = 0;
        queued.to_download = 0;
        assert_eq!(queued.note(), "Waiting to download");

        // And nothing arrived yet is the same answer even once the size is
        // known, which is the state every install spends its first seconds in.
        queued.to_download = 6_000_000_000;
        assert_eq!(queued.note(), "Waiting to download");
        assert_eq!(queued.fraction(), None, "and no bar under it");
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

    /// Whatever a manifest says, an unplayable copy is never offered as one
    /// that can be played — and a bit this shell has never seen changes
    /// nothing.
    ///
    /// This is the honest way to cover the state a full disk leaves behind.
    /// Nobody here knows what `StateFlags` Steam writes when it runs out of
    /// room, and a test that invented a number for it would be asserting
    /// against something made up. What can be said without inventing anything
    /// is the property that matters whatever the number turns out to be: the
    /// only thing that makes a game playable is Steam's own "fully installed"
    /// bit, every other bit can only take that away, and an unknown one is
    /// ignored rather than misread. A disk-full manifest that fails this would
    /// be a Play button on a game that is half on the disk.
    ///
    /// Every combination of the bits the shell does know — all 16384 of them —
    /// which is cheap and leaves nothing to a chosen example.
    /// The states a real install actually passes through, in order, off the
    /// client's own log — and what the shell used to make of the first two.
    ///
    /// Not invented. `content_log.txt` on the machine this was written against
    /// records every change, and a fresh install of a game reads:
    ///
    /// ```text
    /// 21:04:21 AppID 945360 state changed : Update Required,
    /// 21:04:21 AppID 945360 state changed : Update Required,Update Queued,
    /// 21:04:21 AppID 945360 state changed : Update Required,Update Queued,Update Running,
    /// 21:04:28 AppID 945360 state changed : Update Required,Update Queued,Update Running,Update Started,
    /// 21:04:38 AppID 945360 state changed : Fully Installed,Update Queued,Update Running,
    /// 21:04:44 AppID 945360 state changed : Fully Installed,Update Queued,
    /// 21:04:44 AppID 945360 state changed : Fully Installed,
    /// ```
    ///
    /// The first two have no working bit in them, and the shell called both a
    /// **paused download** — which is what a person saw for the first seconds
    /// of every install they started, and which
    /// [`Standing::waiting_for_somebody`] made the shell stop watching on.
    #[test]
    fn the_first_seconds_of_an_install_are_not_a_paused_download() {
        // Update Required, Update Queued, Update Running, Update Started,
        // Fully Installed — the bits behind the words above.
        const REQUIRED: u64 = 2;
        const QUEUED: u64 = 8;
        const RUNNING: u64 = 256;
        const STARTED: u64 = 1024;
        const INSTALLED: u64 = 4;

        let walk = [
            (REQUIRED, 0, Standing::Queued),
            (REQUIRED | QUEUED, 0, Standing::Queued),
            (REQUIRED | QUEUED | RUNNING, 0, Standing::Queued),
            (
                REQUIRED | QUEUED | RUNNING | STARTED,
                60_226_480,
                Standing::Downloading,
            ),
            (
                INSTALLED | QUEUED | RUNNING,
                656_543_488,
                Standing::Updating,
            ),
            (INSTALLED | QUEUED, 656_543_488, Standing::Ready),
            (INSTALLED, 656_543_488, Standing::Ready),
        ];
        for (flags, downloaded, want) in walk {
            assert_eq!(standing_from(flags, downloaded), want, "flags {flags}");
        }

        // None of the way in is a state the shell lets go of the install on.
        // That is the half that cost the most: `Paused` is
        // `waiting_for_somebody`, so the watch ended two seconds after the
        // press and every number on the row afterwards came off the disk alone.
        for (flags, downloaded, _) in walk {
            assert!(
                !standing_from(flags, downloaded).waiting_for_somebody(),
                "flags {flags}"
            );
        }

        // And what the fallback is still for: nothing in flight, not
        // installed, and bytes already on the disk. That is a download that
        // stopped, and it is the one case the first two states above are not.
        assert_eq!(standing_from(REQUIRED, 4_000_000), Standing::Paused);
        assert_eq!(standing_from(0, 4_000_000), Standing::Paused);
    }

    /// The one number the bar is drawn from, and the three ways a manifest can
    /// make it a lie.
    ///
    /// Two programs are writing and reading this file. Steam rewrites it while
    /// the download runs, so a read caught between a new `BytesDownloaded` and
    /// a new `BytesToDownload` can say more has arrived than was asked for —
    /// and a bar drawn past the end of its own groove is a bug somebody sees.
    #[test]
    fn how_far_along_a_download_is_can_only_be_asked_once_there_is_a_total() {
        // No total yet: the client writes the manifest before it knows how
        // large the download is, and nothing is drawn until there is a
        // reading. Nothing, rather than zero — an empty groove at the start of
        // every install reads as a download that has not begun.
        assert_eq!(fraction(0, 0), None);
        assert_eq!(fraction(900, 0), None);

        // Nor is nothing having arrived: "0%" is the absence of a reading
        // rather than one, the shell already has a word for that state, and a
        // row printing it beside an empty groove is what the whole of a short
        // install looked like.
        assert_eq!(fraction(0, 100), None);
        assert_eq!(fraction(25, 100), Some(0.25));
        assert_eq!(fraction(100, 100), Some(1.0));
        // A file caught mid-write.
        assert_eq!(fraction(140, 100), Some(1.0));
        // And a size no f32 can hold exactly, which is every modern game: the
        // answer is still inside its own ends.
        let huge = fraction(90 * 1024 * 1024 * 1024, 100 * 1024 * 1024 * 1024);
        assert!(
            huge.is_some_and(|share| (share - 0.9).abs() < 0.001),
            "{huge:?}"
        );

        // And the same answer off a game — where the standing decides first
        // whether the two numbers are about this game at all.
        let mut game = Game::invented(7, "A Game".to_string(), false);
        assert_eq!(game.fraction(), None);
        game.downloaded = 3;
        game.to_download = 4;
        assert_eq!(game.fraction(), None, "not installed is not a download");
        game.standing = Standing::Downloading;
        assert_eq!(game.fraction(), Some(0.75));

        // The regression this is really about: a game that is simply here.
        // Steam leaves the last operation's byte counts in the manifest, so an
        // installed game reads anything from nought to a full bar off them.
        for (downloaded, to_download) in [(0, 116_304), (1_033_312, 1_033_312)] {
            let mut here = Game::invented(7, "A Game".to_string(), true);
            here.downloaded = downloaded;
            here.to_download = to_download;
            assert_eq!(here.standing, Standing::Ready);
            assert_eq!(here.fraction(), None, "{downloaded}/{to_download}");
        }
    }

    #[test]
    fn nothing_a_manifest_can_say_makes_an_unplayable_copy_look_ready() {
        let known: [u64; 14] = [
            state::FULLY_INSTALLED,
            32,
            128,
            state::UNINSTALLING,
            state::PAUSED,
            state::VALIDATING,
            1024,
            256,
            16384,
            32768,
            65536,
            262144,
            524288,
            1048576,
        ];
        // A bit no version of this shell has ever looked at. Valve's own flags
        // go nowhere near it, so this stands for whatever they add next.
        const NEVER_SEEN: u64 = 1 << 40;

        for combination in 0u32..(1 << known.len()) {
            let flags = known
                .iter()
                .enumerate()
                .filter(|(bit, _)| combination & (1 << bit) != 0)
                .map(|(_, flag)| flag)
                .fold(0u64, |flags, flag| flags | flag);
            for downloaded in [0, 1] {
                let standing = standing_from(flags, downloaded);
                let installed = flags & state::FULLY_INSTALLED != 0;
                assert!(
                    !standing.playable() || installed,
                    "flags {flags} downloaded {downloaded} gave {standing:?}, \
                     which offers to play a copy Steam does not call installed"
                );
                // And the other way round for the one state that means nothing
                // whatever is happening to it.
                if standing == Standing::Ready {
                    assert!(installed, "flags {flags} read as ready");
                }
                // A bit from a Steam that has not been written yet decides
                // nothing. What it must not do is turn one of these into
                // something else quietly.
                assert_eq!(
                    standing_from(flags | NEVER_SEEN, downloaded),
                    standing,
                    "an unknown flag beside {flags} changed what the row says"
                );
            }
        }
    }

    /// A game keeps its place in the column while Steam updates it.
    ///
    /// Reported by the user, watching a game they had just pressed: the row
    /// said "Updating", greyed, **and moved** — down past every other installed
    /// title into the half of the column that is not on the disk, the moment
    /// Steam wrote its first working bit. Coming back to a game to see how far
    /// along it is means finding it again, and it moves back when it finishes.
    ///
    /// The distinction the order was missing is the one [`standing_from`]
    /// already draws: `Updating` is only ever said of a manifest with the
    /// fully-installed bit set, so there is a playable copy on the disk under
    /// it. A first download has no such copy and still sorts with the rest of
    /// what is not here.
    #[test]
    fn a_game_being_updated_does_not_lose_its_place() {
        let installed = |app_id: u32, name: &str, standing: Standing| {
            let mut game = Game::invented(app_id, name.to_string(), true);
            game.standing = standing;
            game.installed = standing.on_the_disk();
            game.updating = standing.moving();
            game
        };
        let order = |games: Vec<Game>| {
            sorted(games)
                .into_iter()
                .map(|game| game.name)
                .collect::<Vec<_>>()
        };

        // Settled: three installed games by name, and the one the account owns
        // and has not got at the end.
        let settled = vec![
            installed(1, "Alpha", Standing::Ready),
            installed(2, "Beta", Standing::Ready),
            installed(3, "Gamma", Standing::Ready),
            Game::invented(4, "Delta".to_string(), false),
        ];
        assert_eq!(order(settled), vec!["Alpha", "Beta", "Gamma", "Delta"]);

        // Beta starts updating. It is exactly where it was.
        let updating = vec![
            installed(1, "Alpha", Standing::Ready),
            installed(2, "Beta", Standing::Updating),
            installed(3, "Gamma", Standing::Ready),
            Game::invented(4, "Delta".to_string(), false),
        ];
        assert_eq!(
            order(updating),
            vec!["Alpha", "Beta", "Gamma", "Delta"],
            "the row moved while somebody was watching it"
        );

        // And a game being fetched for the first time still is not: there is
        // nothing on the disk to play, and it belongs with the rest of what is
        // not here. `installed` is true for it — a manifest exists — which is
        // why this needs the standing rather than that flag.
        let mut fetching = Game::invented(5, "Epsilon".to_string(), true);
        fetching.standing = Standing::Downloading;
        fetching.installed = true;
        fetching.updating = true;
        let coming = vec![
            installed(1, "Alpha", Standing::Ready),
            fetching,
            installed(3, "Gamma", Standing::Ready),
        ];
        assert_eq!(order(coming), vec!["Alpha", "Gamma", "Epsilon"]);

        // And the verify Steam does *first*, which is the other half of the
        // same movement. It full-verifies before any update it has been
        // interrupted in once, so the shape of a real update on this machine
        // is `Validating` for as long as the disk takes and then `Updating` —
        // and a row that counted only the second dropped down the column, sat
        // there through the check, and came back. Twice, for an update that
        // should have moved it not at all.
        let checking = vec![
            installed(1, "Alpha", Standing::Ready),
            installed(2, "Beta", Standing::Validating),
            installed(3, "Gamma", Standing::Ready),
            Game::invented(4, "Delta".to_string(), false),
        ];
        assert_eq!(
            order(checking),
            vec!["Alpha", "Beta", "Gamma", "Delta"],
            "the row moved for the verify before the update"
        );

        // The same word over nothing. A check at the end of a first download
        // has no playable copy under it, and the manifest's own bit is what
        // says so — the standing cannot, because it is the same standing.
        let mut first = Game::invented(5, "Epsilon".to_string(), true).doing(Standing::Validating);
        first.fully_installed = false;
        let arriving = vec![
            installed(1, "Alpha", Standing::Ready),
            first,
            installed(3, "Gamma", Standing::Ready),
        ];
        assert_eq!(order(arriving), vec!["Alpha", "Gamma", "Epsilon"]);

        // Two that are on the disk, still, and not playable, and which the
        // first cut of this order sorted with the games that are: it asked
        // `installed && !updating`, and neither of these is moving. A paused
        // download has nothing to play and a broken copy will not start, so
        // both belong with the rest of what cannot be pressed.
        let stopped = vec![
            installed(1, "Alpha", Standing::Ready),
            installed(2, "Beta", Standing::Paused),
            installed(3, "Gamma", Standing::Broken),
            installed(4, "Delta", Standing::UpdatePaused),
        ];
        assert_eq!(
            order(stopped),
            vec!["Alpha", "Delta", "Beta", "Gamma"],
            "a paused download and a broken copy are not games that can be played"
        );
    }

    /// The builds are what says an update is outstanding, and they say it long
    /// before `StateFlags` does.
    ///
    /// Measured against Valve's own log rather than reasoned about. Switching
    /// Project Zomboid between two branches on this machine, Steam wrote
    /// `StateFlags 6` with a `TargetBuildID` of the build it meant to fetch,
    /// and then did not touch the manifest again for **ten minutes and
    /// thirty-nine seconds** while its own log read `Update Required,Fully
    /// Installed,Update Queued,Update Running,` and it verified the ten
    /// gigabytes already on the disk. Only when bytes began moving did the
    /// manifest pick up `StateFlags 1030`.
    ///
    /// A loading screen is the one thing that has to know about that stretch —
    /// its patience is sixty seconds — so the two build numbers are read.
    #[test]
    fn the_two_build_numbers_say_an_update_is_outstanding_before_the_flags_do() {
        let root = std::env::temp_dir().join(format!(
            "lxb-steam-builds-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let steamapps = root.join("steamapps");
        std::fs::create_dir_all(steamapps.join("common")).expect("a scratch directory");

        let write = |app_id: u32, flags: u32, build: &str, target: &str| {
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
                        "buildid" "{build}"
                        "TargetBuildID" "{target}"
                    }}"#
                ),
            )
            .expect("writable");
            read_manifest(&path, &steamapps).expect("a manifest")
        };

        // Nothing outstanding: the build on the disk is the build Steam wants.
        // Read off this machine either side of the update — 24909800 both
        // ways, with `StateFlags 4`.
        let settled = write(999100, 4, "24909800", "24909800");
        assert!(!settled.update_outstanding);
        assert_eq!(settled.standing, Standing::Ready);

        // The silent stretch. `StateFlags 6` — installed, with an update
        // required — and the two numbers disagreeing is the only thing on the
        // disk that says Steam is going to do something about it.
        let outstanding = write(999101, 6, "24929653", "24909800");
        assert!(outstanding.update_outstanding);
        assert_eq!(
            outstanding.standing,
            Standing::Ready,
            "and the row still says the game is here, because it is"
        );

        // Bytes moving. Still outstanding — it is the same update — and now
        // the flags say so too, which is what the row is drawn from.
        let moving = write(999102, 1030, "24929653", "24909800");
        assert!(moving.update_outstanding);
        assert_eq!(moving.standing, Standing::Updating);

        // The moment between Steam picking the game up and knowing what it
        // wants of it. Read off this machine at 15:19:24, six seconds before
        // the target arrived: a zero is not a build number and must not read
        // as one, or every game Steam has just noticed would look like a game
        // it was about to change.
        let unknown = write(999103, 6, "24909800", "0");
        assert!(!unknown.update_outstanding);

        // The fully-installed bit, carried through as itself. `Ready` implies
        // it, and so does `Updating`; what needs it is the state where the
        // standing cannot say — see [`Sort::InstalledFirst`].
        assert!(settled.fully_installed);
        assert!(moving.fully_installed);

        // And the two fields that say Steam is *not* getting on with it, in
        // the shape they are written on this disk. Neither key present is what
        // every manifest here said before they were read, and it must mean
        // nothing scheduled and nothing gone wrong.
        assert_eq!(outstanding.scheduled_for, 0);
        assert_eq!(outstanding.last_result, 0);

        let more = |app_id: u32, extra: &str| {
            let path = steamapps.join(format!("appmanifest_{app_id}.acf"));
            std::fs::write(
                &path,
                format!(
                    r#""AppState" {{
                        "appid" "{app_id}"
                        "name" "Invented {app_id}"
                        "installdir" "Invented {app_id}"
                        "StateFlags" "6"
                        "SizeOnDisk" "4096"
                        "buildid" "24828357"
                        "TargetBuildID" "24916958"
                        {extra}
                    }}"#
                ),
            )
            .expect("writable");
            read_manifest(&path, &steamapps).expect("a manifest")
        };

        // Deferred. Counter-Strike 2 on this machine, 2026-09-02: `StateFlags
        // 6`, a build two days old, and an appointment for the small hours of
        // the day after tomorrow. The build numbers say the same thing they
        // say for an update in flight, and this is the field that tells the
        // two apart.
        let deferred = more(999104, r#""ScheduledAutoUpdate" "1788405323""#);
        assert!(deferred.update_outstanding);
        assert_eq!(deferred.scheduled_for, 1_788_405_323);
        assert_eq!(deferred.last_result, 0);

        // Kept. Steam leaves the old appointment written after it has been and
        // gone, so the number alone is not a deferral — whether it is in the
        // future needs a clock, and is asked where there is one.
        let past = more(999105, r#""ScheduledAutoUpdate" "1""#);
        assert_eq!(past.scheduled_for, 1);

        // Tried and failed. 4 was seen on this machine on a branch switch that
        // did not take; what any other number means has not been read off a
        // live client, so nothing here reads it as an enumeration.
        let failed = more(999106, r#""UpdateResult" "4""#);
        assert!(failed.update_outstanding);
        assert_eq!(failed.last_result, 4);

        // And the same field while Steam is working, which is the reason it
        // can be read this way at all: it is cleared to 0 when the work is
        // picked up again. Observed 0 at 15:19:24 and again at 15:30:06, both
        // mid-operation, on 2026-09-01.
        let working = more(999107, r#""UpdateResult" "0""#);
        assert_eq!(working.last_result, 0);

        let _ = std::fs::remove_dir_all(&root);
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

        // `done` and `owed` are `BytesDownloaded` and `BytesToDownload`. Both
        // are needed rather than the second alone: what tells an install that
        // has not begun from one that stopped part way is whether anything has
        // arrived.
        let write = |app_id: u32, flags: u32, done: u64, owed: u64| {
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
                        "BytesDownloaded" "{done}"
                    }}"#
                ),
            )
            .expect("writable");
            path
        };

        let ready = read_manifest(&write(999000, 4, 0, 0), &steamapps).expect("a manifest");
        assert!(
            !ready.update_outstanding,
            "a manifest with no TargetBuildID in it wants nothing"
        );
        assert_eq!(ready.app_id, 999000);
        assert_eq!(ready.name, "Invented 999000");
        assert_eq!(ready.size_on_disk, 4096);
        assert_eq!(ready.path, steamapps.join("common").join("Invented 999000"));
        assert_eq!(ready.standing, Standing::Ready);
        assert!(!ready.tool);

        // Installed *and* carrying an update Steam will apply when it starts.
        // The commonest state a big game sits in, and it is playable: reading
        // `StateFlags` as a state rather than a set is what makes half a
        // library look like it is downloading.
        let patched = read_manifest(&write(999001, 4 | 2, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(
            patched.standing,
            Standing::Ready,
            "a game with an update waiting is playable"
        );
        // And the bit itself is kept, because it is the only thing in this
        // manifest that says Steam has something to fetch before it will open
        // a window: the builds do not differ, nothing is scheduled, and the row
        // is right to go on saying Installed. See [`Game::update_required`].
        assert!(patched.update_required);
        assert!(
            !patched.update_outstanding,
            "and the build numbers say nothing at all, which is the case this was written for"
        );
        assert!(!ready.update_required);

        // Asked for and nothing arrived yet. Its own state: the client writes
        // the manifest when it takes the request, and a row reading
        // "Downloading 0%" for the first seconds of every install reads as a
        // stall.
        let queued = read_manifest(&write(999002, 1024, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(queued.standing, Standing::Queued);
        assert!(!queued.standing.playable());
        assert!(queued.standing.moving());

        // On the disk, with an update running against it now. Not the same
        // sentence as a first download: nothing is missing and what is there
        // would still run.
        let busy = read_manifest(&write(999003, 4 | 262144, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(busy.standing, Standing::Updating);
        assert!(busy.standing.moving());

        // Paused with the started bit still set, which is what Steam leaves
        // behind: read as a download that is arriving, the row counts a
        // percentage that will not move again until somebody says so.
        let paused =
            read_manifest(&write(999007, 1024 | 512, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(paused.standing, Standing::Paused);
        assert!(!paused.standing.moving());
        assert!(paused.standing.is_a_download(), "there is one to stop");
        assert!(paused.standing.waiting_for_somebody());
        assert!(!paused.standing.playable());

        // And the same pause over a copy that is already whole, which is a
        // different sentence for the same reason `Updating` is not
        // `Downloading`: nothing is missing and what is there would still run.
        //
        // Not invented. `StateFlags` 516 was read off a real machine — a game
        // that had been played that afternoon, with an update Steam had
        // stopped — and the shell called the row a paused download: no Play on
        // it, and "Stop and Delete" offered against a whole 100 GB copy.
        let held = read_manifest(&write(999011, 4 | 512, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(held.standing, Standing::UpdatePaused);
        assert!(held.standing.playable(), "it was playable an hour ago");
        assert!(held.standing.on_the_disk());
        assert!(!held.standing.moving());
        assert!(
            held.standing.waiting_for_somebody(),
            "nothing will move it but a person"
        );
        assert!(
            !held.standing.is_a_download(),
            "deleting what arrived would delete the game"
        );
        // And it is the one playable state that is not playable offline, which
        // is Valve's rule: its own Offline Mode starts only what is fully up to
        // date. Everything else agrees between the two.
        assert!(!held.standing.playable_offline(), "an update is waiting");
        assert!(Standing::Ready.playable_offline());
        for standing in [
            Standing::NotInstalled,
            Standing::Queued,
            Standing::Downloading,
            Standing::Updating,
            Standing::Validating,
            Standing::Paused,
            Standing::Broken,
            Standing::Uninstalling,
        ] {
            assert_eq!(
                standing.playable(),
                standing.playable_offline(),
                "{standing:?} is the same either way"
            );
        }

        // Begun and stopped with no bit left to say so, which is the other way
        // it happens: a manifest, nothing in flight, not installed, **and
        // bytes on the disk**. Without that last clause this arm caught the
        // first two seconds of every install as well — see the states below.
        let stopped =
            read_manifest(&write(999008, 0, 4_000_000, 9_000_000), &steamapps).expect("a manifest");
        assert_eq!(stopped.standing, Standing::Paused);

        // A check running over a copy that is already there is not a download,
        // and twenty minutes is a long time for a row to say it is one.
        let checking =
            read_manifest(&write(999009, 4 | 131072, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(checking.standing, Standing::Validating);

        // And one on its way off the disk, which outranks everything: it is
        // not installed however installed it still looks.
        let going = read_manifest(&write(999010, 4 | 2048, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(going.standing, Standing::Uninstalling);
        assert!(!going.standing.playable());

        // Files missing: on the disk, and not something to press until Steam
        // has repaired it. It is *on the disk*, which is the half the old pair
        // of booleans got wrong — a broken copy read as absent, so the row
        // offered to fetch a game that was already taking up the space.
        let broken = read_manifest(&write(999004, 4 | 32, 0, 0), &steamapps).expect("a manifest");
        assert_eq!(broken.standing, Standing::Broken);
        assert!(!broken.standing.playable());
        assert!(broken.standing.on_the_disk());
        assert!(broken.standing.waiting_for_somebody());

        // A compatibility tool declares itself with the file Steam finds it
        // by, and never becomes a row of its own.
        let tool_dir = steamapps.join("common").join("Invented 999005");
        std::fs::create_dir_all(&tool_dir).expect("writable");
        std::fs::write(tool_dir.join("toolmanifest.vdf"), "\"manifest\" {}").expect("writable");
        let tool = read_manifest(&write(999005, 4, 0, 0), &steamapps).expect("a manifest");
        assert!(tool.tool, "Proton and the Linux runtimes are not games");

        // Nor are the redistributables Steam installs beside games.
        let shared = steamapps.join("common").join("Invented 999006");
        std::fs::create_dir_all(shared.join("_CommonRedist")).expect("writable");
        let redist = read_manifest(&write(999006, 4, 0, 0), &steamapps).expect("a manifest");
        assert!(redist.tool);

        let _ = std::fs::remove_dir_all(&root);
    }
}
