//! Windows Apps, as the shell holds it: one row, one column, and the games
//! somebody has installed into prefixes of their own.
//!
//! Everything that touches the machine — which Proton is here, fetching one,
//! making a prefix, working out how to start a game, taking one off the disk
//! — is a separate program, `lxb-proton`, which ships in a package of its
//! own, exactly as `lxb-heroic` and `lxb-rpcs3` do. **This module is the half
//! that belongs to the shell**, and where that program is not on the machine
//! every method here answers as though the integration did not exist. See
//! `crates/lxb-proton`, and `PROTON-INTEGRATION-TO-DO.MD` for everything that
//! was measured before any of it was written.
//!
//! ## The journey, as the row
//!
//! ```text
//!   press Windows Apps ─► no Proton ─► "Download it?" ─► a Proton and its runtime ─┐
//!                      └─ a Proton here ──────────────────────────────────────────►│
//!                                                                                  ▼
//!                                                               the Windows Apps column
//! ```
//!
//! With Steam on the machine there is usually nothing to fetch at all: the
//! Protons Steam installed are read where they lie, and the first press opens
//! the column. The download is for the machine that has no Steam, which is
//! the whole reason this integration exists as something other than a
//! shortcut into Valve's client.
//!
//! ## Why the shell starts the games itself
//!
//! The helper works out *what to run* — the runtime wrapping the Proton
//! wrapping the game, and the environment it needs — and hands it over. The
//! shell runs it, so a Windows game gets the loading screen, the display it
//! was pressed on and the guide's Close, exactly as an Epic game or a ROM
//! does.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

use crate::apps::{self, Entry};
use crate::icons;

/// What the helper is called, and what the shell looks for on `PATH`.
pub const HELPER: &str = "lxb-proton";

/// The protocol revision this shell was written against. See
/// `crates/lxb-proton/src/report.rs`, which declares the same number.
pub const PROTOCOL: u32 = 1;

/// What the row and the column are called.
///
/// A name rather than a sentence, so it is not translated — the same as
/// Steam's, RetroArch's, Epic Games' and the PlayStation 3's.
pub const TITLE: &str = "Windows Apps";

/// The X11 class every Windows game this integration starts puts on its
/// windows. What a game's Resolution is sent under — see
/// `Shell::resolution_subject`.
///
/// Read out of Wine's own X11 driver rather than guessed, and then measured
/// against a real Proton game on a real display. The driver has exactly two
/// paths and no third: `steam_app_<id>` when `SteamGameId` or `SteamAppId` is
/// in the environment, and this literal string when neither is. There is no
/// variable that overrides it.
///
/// So every game here shares one class, and that is not a thing to fix by
/// inventing an app id — see `crates/lxb-proton/src/launch.rs`, where what a
/// fabricated one costs is written out. It is instead the same shape a ROM
/// folder has: one name, every game played under it, and the answer for the
/// game being started pushed on the way into *its* launch. See
/// [`crate::resolution::Subject::Game`], whose note is about the same
/// problem, and `Shell::push_resolution_for_a_launch`.
pub const WINDOW_CLASS: &str = "steam_proton";

/// The mark the row, the column and every game without a picture of its own
/// wear. It arrives with the package, under `share/lxb/glyphs`.
const MARK: &str = "lxb:windows";

/// How a Windows game is named everywhere the shell keeps something about
/// one — its resolution, its pointer settings, its place in a sort order.
///
/// `windows:<game-id>`, and never a number. A numeric identifier is read
/// elsewhere in this shell as a Steam app id — see
/// `game_a_window_is_named_after` — so a Windows game wearing one would be
/// filed as somebody's Steam game.
pub fn key(id: &str) -> String {
    format!("windows:{id}")
}

/// The mark, or the shell's own pad where the package brought no drawing.
pub fn mark() -> &'static str {
    if icons::shaped(MARK) {
        MARK
    } else {
        icons::CATEGORY_GAMES
    }
}

// --- where the helper is ----------------------------------------------------

static FOUND: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where new games are kept, as the helper last said.
///
/// The helper owns this setting — it keeps it in a file of its own and reads
/// it back on every launch — so what is here is a copy for drawing with, not
/// a second place it is decided. Settings builds its row from free functions
/// with no shell to ask, which is the whole reason this is a static rather
/// than a field; see [`folder_row`].
static ROOTS: Mutex<Option<String>> = Mutex::new(None);

/// The shell's hidden `--proton-helper`, where it named one.
static NAMED: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Whether this machine has the package — which decides whether there is a
/// Windows Apps row, column and Settings page at all.
///
/// The package alone, and never the switch. Settings asks this to decide
/// whether there is a page, and the page then asks
/// [`crate::settings::windows_integration`] to decide what is on it: a page
/// gated on the setting would be a page that took its own switch away with
/// it, and an integration nobody could turn back on.
pub fn offered() -> bool {
    FOUND.lock().is_ok_and(|found| found.is_some())
}

/// The helper, where this machine has it — for starting the integration
/// again once it has been switched off and on.
pub fn helper() -> Option<PathBuf> {
    FOUND.lock().ok()?.clone()
}

/// Whether this machine runs Windows programs at all: the package is here,
/// and the integration has not been switched off.
///
/// Both halves, where [`offered`] is only the first. The two answer
/// different questions and Settings needs them apart — a page gated on the
/// switch would be a page that took its own switch away with it — but
/// anything about *running* a program needs them together.
///
/// What asks is everything outside this module that meets a Windows program
/// where no column put it: a `.exe` sitting in a folder of the file
/// explorer. Both facts are read at the question rather than remembered, so
/// a package installed or an integration switched back on part way through a
/// session is answered by the next listing.
pub fn runs_windows_programs() -> bool {
    offered() && crate::settings::windows_integration()
}

/// Say that this machine has the package, for a test that builds a page which
/// only exists where it does.
///
/// The Settings tree is built by free functions that reach for this module's
/// facts, so a test of one of those pages has to be able to state the fact
/// first — the same shape [`crate::retroarch::set_core_marks`] is, and the
/// reason `FOUND` is not simply made public. Put it back to `None` when the
/// test is done: it is one static for the whole suite.
#[cfg(test)]
pub fn pretend_installed(at: Option<PathBuf>) {
    *FOUND.lock().unwrap_or_else(|err| err.into_inner()) = at;
}

/// Held for as long as a test is pretending, by every test that pretends.
///
/// [`FOUND`] is one static for the whole binary and the tests run in
/// parallel, so without this a test that said the package was here would be
/// deciding what a test in another module started from — and the ones that
/// read it are in other modules, because the whole point of the fact is that
/// the rest of the shell asks it. The same arrangement `settings`' own tests
/// and [`crate::theme::with_accent`] use, and for the same reason.
#[cfg(test)]
pub static PRETENDING: Mutex<()> = Mutex::new(());

/// Look for the helper: `named` first, then beside the shell's own
/// executable, then `PATH` — [`crate::heroic::look_for_helper`]'s order, for
/// its reason: a local build must not pick up an older installed helper.
pub fn look_for_helper(named: Option<PathBuf>) -> Option<PathBuf> {
    let _ = NAMED.set(named);
    let found = find_helper(true);
    match &found {
        Some(at) => tracing::info!(at = %at.display(), "the Windows Apps integration is installed"),
        None => tracing::debug!("no Windows Apps integration on this machine"),
    }
    if let Ok(mut held) = FOUND.lock() {
        *held = found.clone();
    }
    found
}

/// Look for the package again, and answer whether it has come or gone since.
///
/// Driven by the shell's own five-second sweep, so installing or removing the
/// package while a session runs grows or drops the column without anybody
/// having to sign out.
pub fn look_again() -> bool {
    let found = find_helper(false);
    let Ok(mut held) = FOUND.lock() else {
        return false;
    };
    let changed = *held != found;
    if changed {
        match &found {
            Some(at) => tracing::info!(at = %at.display(), "the Windows Apps package arrived"),
            None => tracing::info!("the Windows Apps package has gone"),
        }
    }
    *held = found;
    changed
}

fn find_helper(say: bool) -> Option<PathBuf> {
    if let Some(named) = NAMED.get().and_then(Option::as_ref) {
        return executable(named).or_else(|| {
            if say {
                tracing::warn!(at = %named.display(), "--proton-helper is not a program here");
            }
            None
        });
    }
    // Beside the shell's own binary before `PATH`: a shell run out of a build
    // directory must use the helper built beside it rather than an older one
    // somebody installed, or a change to the pair cannot be looked at at all.
    if let Some(beside) = std::env::current_exe()
        .ok()
        .and_then(|at| at.parent().map(|dir| dir.join(HELPER)))
        .and_then(|at| executable(&at))
    {
        return Some(beside);
    }
    on_path(HELPER)
}

/// That path, if it is a program this user can run.
fn executable(at: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(at).ok()?;
    (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then(|| at.to_path_buf())
}

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| executable(&dir.join(program)))
}

/// Ask the helper for the Windows mark, for a machine whose
/// `share/lxb/glyphs` does not have it — the helper run from where it was
/// built, which is how the shell finds it on a development machine. Once,
/// while the shell starts; see [`icons::bring_mark`].
pub fn bring_the_mark(helper: &Path) {
    #[derive(Deserialize)]
    struct Mark {
        protocol: u32,
        name: String,
        drawing: String,
    }
    let said = Command::new(helper)
        .arg("mark")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let mark = said
        .ok()
        .filter(|said| said.status.success())
        .and_then(|said| serde_json::from_slice::<Mark>(&said.stdout).ok());
    match mark {
        Some(mark) if mark.protocol == PROTOCOL && format!("lxb:{}", mark.name) == MARK => {
            icons::bring_mark(MARK, mark.drawing);
        }
        _ => tracing::debug!(helper = %helper.display(), "the Windows Apps helper gave no mark"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(stage: Stage) -> Progress {
        Progress {
            protocol: PROTOCOL,
            stage,
            progress: Some(0.5),
            trouble: None,
        }
    }

    /// **A press on a `.exe` names what it plays before anything has run**,
    /// and names it where the person keeps it.
    ///
    /// Checked against the line itself rather than against the parser above,
    /// because what the two programs agree on is the JSON: a field renamed on
    /// either side is a press that quietly does something else.
    #[test]
    fn a_new_prefix_plays_the_program_where_it_already_is() {
        let made = serde_json::json!({
            "protocol": PROTOCOL,
            "game": {
                "id": "notepad",
                "name": "Notepad++",
                "state": "ready",
                "at": "/home/someone/.lxb-proton/notepad",
                "play": "/home/someone/Desktop/npp.portable/notepad++.exe",
                "runs_in_place": true,
                "prefix_built": false,
            },
            "start": {
                "protocol": PROTOCOL,
                "argv": ["/proton", "waitforexitandrun", "notepad++.exe"],
                "environment": [],
                "directory": "/home/someone/Desktop/npp.portable",
                "proton": "Proton 11.0",
                "trouble": serde_json::Value::Null,
                "note": "Notepad++ starting",
            },
            "trouble": serde_json::Value::Null,
            "note": "notepad plays it",
        })
        .to_string();

        let made: Made = serde_json::from_str(&made).expect("the launch, and the game it is for");
        let game = made.game.expect("it made one");
        assert_eq!(
            game.play.as_deref(),
            Some("/home/someone/Desktop/npp.portable/notepad++.exe"),
            "the program is what the game plays from the first frame, so the \
             row is never one that says nothing has been installed"
        );
        assert!(
            game.runs_in_place,
            "and the shell is told this prefix is one to settle when the run ends"
        );
        assert_eq!(
            made.start.expect("a launch").directory,
            "/home/someone/Desktop/npp.portable",
            "run in its own folder, with everything it came with beside it"
        );
    }

    /// **A Notepad++ folder was imported on this machine, landed complete on
    /// the disk, and left a row saying *Nothing has been installed here
    /// yet*.** Not one line of that import ever reached the shell.
    ///
    /// There were two counters called `generation`, one here and one in the
    /// worker, and they counted different things: the worker counted every
    /// long job and this side counted only the ones somebody gave up on. So
    /// they disagreed from the very first import — the worker stamped its
    /// lines `1`, this side was still comparing against `0` — and every
    /// line was discarded as belonging to a job nobody had asked for.
    /// Including `done`, which is the line that reads the column again, so
    /// the row kept whatever it had said before until the shell was
    /// restarted.
    ///
    /// They are one counter now, and this is what it has to do: keep the
    /// lines of the job in hand, from the very first one, and drop the
    /// lines of a job somebody has abandoned.
    #[test]
    fn the_lines_of_a_first_import_are_not_thrown_away_as_somebody_elses() {
        let (mut proton, says, generation) = Proton::listening();
        let before = generation.load(Ordering::SeqCst);
        proton.send(Ask::Import {
            dir: PathBuf::from("/home/someone/Desktop/a portable game"),
            name: None,
        });
        // What the worker stamps a line with is what it reads as the job
        // starts — and the job is counted *here*, where it was asked for,
        // which is the half that was missing. Both sides read this one
        // number or neither of them does.
        let stamp = generation.load(Ordering::SeqCst);
        assert_eq!(stamp, before + 1, "asking for an import is a new job");

        let _ = says.send(Heard::Working(stamp, progress(Stage::Settling)));
        proton.poll();
        assert_eq!(
            proton.doing().map(|doing| doing.stage),
            Some(Stage::Settling),
            "the bar moves while the game is copied in"
        );

        let _ = says.send(Heard::Working(stamp, progress(Stage::Done)));
        let change = proton.poll();
        assert_eq!(
            change.finished,
            Some(Finished::Fetched),
            "and the end of it is heard, which is what reads the column again"
        );
        assert!(proton.doing().is_none(), "nothing is still going on");

        // And the thing the counter is actually for: a job somebody gave up
        // on cannot paint over what came after it.
        proton.send(Ask::Import {
            dir: PathBuf::from("/home/someone/Desktop/another one"),
            name: None,
        });
        let abandoned = generation.load(Ordering::SeqCst);
        proton.stop_working();
        let _ = says.send(Heard::Working(abandoned, progress(Stage::Settling)));
        proton.poll();
        assert!(
            proton.doing().is_none(),
            "a line from a job nobody is waiting for any more says nothing"
        );
    }

    /// A Windows game is never named by a number, because a number means
    /// something else here: `game_a_window_is_named_after` reads any
    /// `steam_app_<n>` back as a Steam title.
    #[test]
    fn a_windows_games_name_could_never_be_read_as_a_steam_one() {
        let named = key("stardew-valley");
        assert_eq!(named, "windows:stardew-valley");
        assert!(named.parse::<u32>().is_err());
        assert!(!named.starts_with("steam_app_"));
    }

    fn row(name: &str, ready: bool) -> GameRow {
        GameRow {
            id: name.to_lowercase(),
            name: name.to_string(),
            state: match ready {
                true => GameState::Ready,
                false => GameState::Unfinished,
            },
            proton: None,
            at: format!("/games/{}", name.to_lowercase()),
            play: ready.then(|| "pfx/drive_c/game.exe".to_string()),
            prefix_built: ready,
            runs_in_place: false,
            cover: None,
        }
    }

    fn listed(sort: Sort, mut games: Vec<GameRow>) -> Vec<String> {
        games.sort_by(|one, two| sort.compare(one, two));
        games.into_iter().map(|game| game.name).collect()
    }

    /// The default order: a game there is something to play comes before one
    /// whose setup never finished, and each half runs down the alphabet.
    ///
    /// The half-installed game last is the point of it. It is the row
    /// somebody is least likely to want and the one most likely to be pressed
    /// on the way past, and it cannot be started at all.
    #[test]
    fn the_games_that_can_be_played_are_listed_before_the_ones_that_cannot() {
        let games = vec![
            row("Anna", false),
            row("Beatrix", true),
            row("Carmen", false),
            row("Dorothy", true),
        ];
        assert_eq!(
            listed(Sort::ReadyFirst, games),
            ["Beatrix", "Dorothy", "Anna", "Carmen"]
        );
    }

    /// Both ways down the alphabet, and neither of them half the alphabet
    /// twice: somebody who picks a name order is looking a game up by name,
    /// and whether it is installed is not what they asked.
    #[test]
    fn a_name_order_is_one_pass_through_the_alphabet_not_two() {
        let games = || {
            vec![
                row("Carmen", true),
                row("anna", false),
                row("Beatrix", true),
            ]
        };
        assert_eq!(
            listed(Sort::NameAscending, games()),
            ["anna", "Beatrix", "Carmen"],
            "and a lower-case name sorts with its letter, not before every letter"
        );
        assert_eq!(
            listed(Sort::NameDescending, games()),
            ["Carmen", "Beatrix", "anna"]
        );
    }

    /// Every order survives a trip through the settings file, and a name the
    /// file holds that this column does not offer is nothing rather than a
    /// panic — a hand-edited typo, or a setting from a later version.
    #[test]
    fn an_order_comes_back_from_the_settings_file_and_a_strange_one_does_not() {
        for sort in SORTS {
            assert_eq!(Sort::from_key(sort.key()), Some(*sort));
        }
        assert_eq!(Sort::from_key("recently-played"), None);
        assert_eq!(Sort::from_key(""), None);
    }

    /// The three keys are the Steam column's own words for the same three
    /// orders, so a person who has met one list has met this one.
    #[test]
    fn the_orders_are_named_the_way_the_rest_of_the_bar_names_them() {
        assert_eq!(Sort::ReadyFirst.key(), "installed-first");
        assert_eq!(Sort::NameAscending.key(), "name");
        assert_eq!(Sort::NameDescending.key(), "name-reversed");
        assert_eq!(Sort::default(), Sort::ReadyFirst);
    }

    fn probe(proton: Option<Tool>, steam: bool) -> Probe {
        Probe {
            protocol: PROTOCOL,
            proton,
            steam,
            prefix_roots: Vec::new(),
        }
    }

    fn a_usable_proton() -> Tool {
        Tool {
            name: "Proton 11.0".into(),
            bin: "/steam/common/Proton 11.0/proton".into(),
            origin: Origin::Valve,
            version: None,
            runtime: Some(Runtime {
                suite: Some("steamrt4".into()),
                entry_point: Some("/steam/common/SteamLinuxRuntime_4/_v2-entry-point".into()),
                version: None,
            }),
        }
    }

    /// The two rows at the head of the column both open a column, and they
    /// stay at the head whether or not there is a game under them.
    ///
    /// Neither is a row kind of its own, and that is the point: a row that
    /// opens a column *is* a folder row here, so the shell's own navigation
    /// walks both and no press plumbing was needed for either.
    ///
    /// **Two, and there were three.** The third reached into a game that is
    /// already set up, to run its patcher or its settings program — a thing
    /// somebody does once a year, standing at the head of the column above
    /// every game, every day. It is on a game's own menu now.
    #[test]
    fn the_column_is_two_ways_in_and_then_the_games() {
        let empty = Proton::holding(
            Some(probe(Some(a_usable_proton()), false)),
            Vec::new(),
            Vec::new(),
        );
        let rows = empty.rows(None);
        assert_eq!(rows.len(), 2, "the two ways in exist before the first game");
        assert_eq!(crate::apps::head_rows(&rows), 2);
        assert!(
            rows.iter().all(|row| row.folder().is_some()),
            "both of them are folder rows"
        );

        let with_games = Proton::holding(
            Some(probe(Some(a_usable_proton()), false)),
            Vec::new(),
            vec![row("Beatrix", true), row("Anna", false)],
        );
        let rows = with_games.rows(None);
        assert_eq!(
            crate::apps::head_rows(&rows),
            2,
            "and they are still the head"
        );
        assert_eq!(
            rows[2..]
                .iter()
                .filter_map(|row| row.windows_game())
                .map(|game| game.name.as_str())
                .collect::<Vec<_>>(),
            ["Beatrix", "Anna"],
            "with the games under them in the order the column is sorted in"
        );
    }

    /// **The walk is the same rows off the same disk whichever way it was
    /// opened, so the row it hangs off is the only thing that can say which
    /// one this is.**
    ///
    /// Somebody standing in their own Downloads, having asked to run a mod's
    /// setup inside Cemu, has to be able to see that Cemu is where what they
    /// pick is going — otherwise it reads exactly like the row above it that
    /// gives a program a Windows of its own, which is the opposite.
    #[test]
    fn the_row_a_walk_hangs_off_says_which_game_it_is_for() {
        let games = vec![row("Cemu", true), row("Notepad++", true)];
        let holding = |inside: Option<&str>| {
            let proton = Proton::holding(
                Some(probe(Some(a_usable_proton()), false)),
                Vec::new(),
                games.clone(),
            );
            let rows = proton.rows(inside);
            let folder = rows[0]
                .folder()
                .expect("the row the walk hangs off")
                .clone();
            (folder.title, folder.comment)
        };

        let (title, comment) = holding(None);
        assert_eq!(title, "Run a Windows program", "opened from the column");
        assert!(comment.is_some(), "and it says what it will do with a pick");

        let (title, comment) = holding(Some("cemu"));
        assert_eq!(title, "Run inside Cemu", "opened from Cemu's own menu");
        assert_eq!(
            comment, None,
            "the title is the whole of it; a second line would be the row twice"
        );

        // A game that went off the disk while the walk was open is no game
        // to name, and the row is everybody's again rather than a row with
        // an empty name on it.
        let (title, _) = holding(Some("uninstalled-since"));
        assert_eq!(title, "Run a Windows program");
    }

    /// A column of nothing but head rows opens on the first of them — the one
    /// that walks the disk for a program — and that is why
    /// `crate::model::first_row` carries an exception at all.
    #[test]
    fn an_empty_column_opens_on_the_row_that_walks_the_disk() {
        let empty = Proton::holding(
            Some(probe(Some(a_usable_proton()), false)),
            Vec::new(),
            Vec::new(),
        );
        let rows = empty.rows(None);
        assert_eq!(
            rows.iter()
                .position(crate::apps::Entry::opens_an_empty_column),
            Some(0),
            "the first row is the one the column has to open on"
        );
    }

    /// Compatibility is a list of Protons with the machine's own choice at the
    /// top, the one in force ticked, and a Proton whose runtime is missing
    /// greyed out rather than left to fail inside a container.
    #[test]
    fn the_compatibility_list_leads_with_giving_a_game_its_default_back() {
        let mut broken = a_usable_proton();
        broken.name = "Proton 9.0".into();
        broken.runtime = None;
        let mut chose_one = row("Beatrix", true);
        chose_one.proton = Some("Proton 9.0".into());
        let proton = Proton::holding(
            Some(probe(Some(a_usable_proton()), true)),
            vec![a_usable_proton(), broken],
            vec![chose_one],
        );
        let rows = proton.compatibility_rows("beatrix");
        assert_eq!(
            rows.first().map(|row| row.command),
            Some(crate::menu::Command::WindowsUnder(None)),
            "the row that takes a game's own choice back comes first"
        );
        assert_eq!(
            rows[2].command,
            crate::menu::Command::WindowsUnder(Some("Proton 9.0")),
            "and the chosen one is in the list by name"
        );
        assert!(
            rows[2].reading,
            "a Proton with no runtime on this machine cannot start anything, and says so"
        );
        assert!(
            !rows[1].reading,
            "while the one that can is a row somebody can press"
        );
    }

    /// Nothing has been asked yet: the list says it is looking rather than
    /// claiming this machine has no Proton at all.
    #[test]
    fn a_compatibility_list_that_has_not_arrived_says_it_is_looking() {
        let proton = Proton::holding(
            Some(probe(None, true)),
            Vec::new(),
            vec![row("Beatrix", true)],
        );
        let rows = proton.compatibility_rows("beatrix");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].reading, "and it is not a row anything happens on");
    }

    /// A display is written the one way the helper's `--desktop` reads one.
    /// The two halves are in different crates and this is the whole of what
    /// they agree about, so it is written down on this side too.
    #[test]
    fn a_display_is_written_across_by_down() {
        assert_eq!(a_display((1280, 800)), "1280x800");
        assert_eq!(a_display((3840, 2160)), "3840x2160");
    }

    fn said(ask: Ask) -> Vec<String> {
        arguments(&ask)
            .into_iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect()
    }

    /// **Every launch says which display it is opening on**, because a setup
    /// is given a Windows desktop that size and a desktop of the wrong size
    /// is an installer somewhere other than the middle of the screen.
    ///
    /// Said for the game itself too, and the helper is the one that decides a
    /// game does not get a desktop — one rule, in one place, rather than two
    /// halves of a rule that can drift apart.
    #[test]
    fn a_launch_says_how_big_the_display_it_opens_on_is() {
        assert_eq!(
            said(Ask::Start {
                id: "notepad".into(),
                inside: None,
                compatibility: None,
                desktop: Some((1280, 800)),
            }),
            ["start", "notepad", "--desktop", "1280x800"]
        );
        assert_eq!(
            said(Ask::Start {
                id: "cemu".into(),
                inside: Some(PathBuf::from("/home/someone/Downloads/mod.exe")),
                compatibility: None,
                desktop: Some((3840, 2160)),
            }),
            [
                "start",
                "cemu",
                "--in",
                "/home/someone/Downloads/mod.exe",
                "--desktop",
                "3840x2160",
            ]
        );
        assert_eq!(
            said(Ask::New {
                exe: PathBuf::from("/home/someone/Desktop/setup.exe"),
                name: None,
                desktop: Some((1920, 1080)),
            }),
            [
                "new",
                "/home/someone/Desktop/setup.exe",
                "--desktop",
                "1920x1080",
            ]
        );
    }

    /// Ending what is left of a game's Windows is one word and the game, and
    /// so is asking what the run in it left behind.
    #[test]
    fn ending_a_games_windows_names_only_the_game() {
        assert_eq!(said(Ask::End { id: "cemu".into() }), ["end", "cemu"]);
        assert_eq!(
            said(Ask::Settled {
                id: "notepad".into()
            }),
            ["settled", "notepad"]
        );
    }

    /// **A helper too old to know a verb must not put a sentence on screen.**
    ///
    /// The two halves of this integration are separate packages, so a shell
    /// that has been updated can be talking to a helper that has not, and
    /// `clap` answers a verb it never heard of by printing nothing — which
    /// this module otherwise reads as the helper being broken. Every press
    /// has somebody waiting on it and must say so; the tidying-up after a
    /// launch has nobody, and without this every Windows application that
    /// closed on such a machine put *something went wrong* on the screen
    /// behind it.
    #[test]
    fn only_the_asks_somebody_pressed_are_answered_on_screen() {
        assert!(!anybody_is_waiting(&Ask::End { id: "cemu".into() }));
        assert!(!anybody_is_waiting(&Ask::Settled {
            id: "notepad".into()
        }));
        for pressed in [
            Ask::Probe,
            Ask::Games,
            Ask::Offer,
            Ask::Fetch,
            Ask::Remove { id: "cemu".into() },
            Ask::FoundIn { id: "cemu".into() },
            Ask::Start {
                id: "cemu".into(),
                inside: None,
                compatibility: None,
                desktop: None,
            },
            Ask::New {
                exe: PathBuf::from("/d/setup.exe"),
                name: None,
                desktop: None,
            },
        ] {
            assert!(anybody_is_waiting(&pressed), "{pressed:?}");
        }
    }

    /// A shell that does not know its display yet says nothing about one, and
    /// the helper falls back to running against the screen itself. A size
    /// nobody measured would be a desktop nobody can see the edges of.
    #[test]
    fn a_launch_before_the_first_configure_names_no_display() {
        assert_eq!(
            said(Ask::New {
                exe: PathBuf::from("/home/someone/Desktop/setup.exe"),
                name: Some("Notepad++".into()),
                desktop: None,
            }),
            [
                "new",
                "/home/someone/Desktop/setup.exe",
                "--name",
                "Notepad++"
            ]
        );
    }

    /// The class every Windows game's windows carry, and the one thing it
    /// must never be: something `game_a_window_is_named_after` reads back as
    /// a Steam title.
    ///
    /// Measured against Wine's own X11 driver, which holds exactly two
    /// strings for this and no third — `steam_app_%s` and this one — so a
    /// game started with no app id gets this and nothing else.
    #[test]
    fn the_window_class_is_not_a_steam_app_id() {
        assert_eq!(WINDOW_CLASS, "steam_proton");
        assert!(!WINDOW_CLASS.starts_with("steam_app_"));
        assert!(
            WINDOW_CLASS.strip_prefix("steam_app_").is_none(),
            "a class that parsed as steam_app_<n> would file this game as Steam's"
        );
    }

    /// A new field on the wire is one an older shell can do without: the
    /// helper that does not report what a game plays is a helper whose games
    /// still list.
    #[test]
    fn a_game_row_without_the_newer_fields_still_reads() {
        let said = r#"{"id":"clock-game","name":"Clock Game","state":"ready",
                       "at":"/games/clock-game"}"#;
        let row: GameRow = serde_json::from_str(said).expect("the older shape still reads");
        assert_eq!(row.play, None);
        assert!(!row.prefix_built);
        assert_eq!(row.proton, None);
    }
}

// --- what the helper says ---------------------------------------------------
//
// Written out here rather than shared with the helper's crate, exactly as
// every other integration does it: the two are installed apart, and what they
// share is a wire format declared on both sides. Every failure enum carries
// `#[serde(other)] Other` so a word from a newer helper degrades into "try
// again" instead of failing the parse, and every field a newer helper might
// not write carries `#[serde(default)]`.

/// One Proton this machine could run a Windows game with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tool {
    pub name: String,
    pub bin: String,
    pub origin: Origin,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub runtime: Option<Runtime>,
}

impl Tool {
    /// Whether it could start a game today, which is whether the runtime it
    /// asks for is on this machine.
    pub fn usable(&self) -> bool {
        self.runtime
            .as_ref()
            .is_some_and(|runtime| runtime.entry_point.is_some())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Valve,
    Custom,
    Managed,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Runtime {
    #[serde(default)]
    pub suite: Option<String>,
    #[serde(default)]
    pub entry_point: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Mirrors of the helper's records carry every field the wire carries, read
/// or not: what the two halves agree on is the record, and a field dropped
/// because today's shell has no use for it is a field the next change has to
/// rediscover. See `crates/lxb-proton/src/report.rs`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Probe {
    pub protocol: u32,
    #[serde(default)]
    pub proton: Option<Tool>,
    #[serde(default)]
    pub steam: bool,
    #[serde(default)]
    pub prefix_roots: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Tools {
    pub protocol: u32,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub chosen: Option<String>,
    #[serde(default)]
    pub steam: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Games {
    pub protocol: u32,
    #[serde(default)]
    pub games: Vec<GameRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GameRow {
    pub id: String,
    pub name: String,
    pub state: GameState,
    #[serde(default)]
    pub proton: Option<String>,
    pub at: String,
    /// What a press of Play runs, as a path inside the game's own folder, or
    /// `None` while nothing has been named. The menu that offers to change it
    /// ticks this one.
    #[serde(default)]
    pub play: Option<String>,
    #[serde(default)]
    pub prefix_built: bool,
    /// Whether what it plays is a program outside its own folder — one
    /// pressed in Files and run where the person keeps it.
    ///
    /// One thing is read off it and nothing else: whether a launch ending in
    /// this prefix is one worth asking [`Ask::Settled`] about. What is then
    /// done about it belongs to the helper, which is the half that has the
    /// prefix.
    #[serde(default)]
    pub runs_in_place: bool,
    /// The program's own drawing, which the helper took out of the
    /// executable and wrote beside the game. A file on this disk, so it goes
    /// through the same worker and the same band of the same atlas a
    /// photograph does — see [`crate::apps::WindowsGame::cover`].
    #[serde(default)]
    pub cover: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GameState {
    Ready,
    #[serde(other)]
    Unfinished,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Start {
    pub protocol: u32,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub environment: Vec<(String, String)>,
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub trouble: Option<Trouble>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Made {
    pub protocol: u32,
    #[serde(default)]
    pub game: Option<GameRow>,
    #[serde(default)]
    pub start: Option<Start>,
    #[serde(default)]
    pub trouble: Option<Trouble>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Found {
    pub protocol: u32,
    pub id: String,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Candidate {
    pub at: String,
    pub name: String,
    #[serde(default)]
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Offer {
    pub protocol: u32,
    #[serde(default)]
    pub proton: Option<Download>,
    #[serde(default)]
    pub runtime: Option<Download>,
    #[serde(default)]
    pub trouble: Option<Trouble>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Download {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub already: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    pub protocol: u32,
    pub stage: Stage,
    #[serde(default)]
    pub progress: Option<f32>,
    #[serde(default)]
    pub trouble: Option<Trouble>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Looking,
    Proton,
    Runtime,
    Checking,
    Settling,
    Done,
    Failed,
    #[serde(other)]
    Other,
}

/// What the helper says it left of a game's Windows. Nothing on the bar
/// changes for it: a launch that is over is over either way, and what this
/// answers is the log's question rather than the user's.
#[derive(Debug, Clone, Deserialize)]
pub struct Ended {
    pub protocol: u32,
    pub id: String,
    #[serde(default)]
    pub found: u32,
    #[serde(default)]
    pub killed: u32,
    #[serde(default)]
    pub left: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Removal {
    pub protocol: u32,
    pub id: String,
    #[serde(default)]
    pub removed: bool,
    #[serde(default)]
    pub trouble: Option<Trouble>,
}

/// What went wrong, as a word. Turned into a sentence in the person's own
/// language by [`trouble_sentence`] and never drawn as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trouble {
    Offline,
    Damaged,
    NoSpace,
    Files,
    WrongMachine,
    UnknownRuntime,
    InUse,
    Gone,
    #[serde(other)]
    Other,
}

/// What the person is told when something did not happen.
///
/// Every one of these says what they can do about it and names no component
/// of the machine — not a runtime, not a digest, not a path. The log has
/// those.
pub fn trouble_sentence(trouble: Trouble) -> &'static str {
    match trouble {
        Trouble::Offline => crate::i18n::text("windows-trouble-offline"),
        Trouble::Damaged => crate::i18n::text("windows-trouble-damaged"),
        Trouble::NoSpace => crate::i18n::text("windows-trouble-no-space"),
        Trouble::WrongMachine => crate::i18n::text("windows-trouble-wrong-machine"),
        Trouble::InUse => crate::i18n::text("windows-trouble-in-use"),
        Trouble::Gone => crate::i18n::text("windows-trouble-gone"),
        Trouble::Files | Trouble::UnknownRuntime | Trouble::Other => {
            crate::i18n::text("windows-trouble-other")
        }
    }
}

/// The orders the Windows Apps column can be listed in.
///
/// Three, not the Steam library's eight, and the reason is the data rather
/// than the effort: a Windows game here has a name and whether there is
/// something to play, and that is all. Nobody's play time is recorded, and
/// the size of a prefix is the size of a Windows installation rather than of
/// a game, so an order by either would be a row somebody chooses and then
/// cannot trust. See `lxb_steam::library::Sort`, where the full eight belong
/// to a library that knows all eight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// Everything there is something to play, then everything half-installed,
    /// each half by name.
    ///
    /// The default, and for the reason the Steam column has the same one: an
    /// unfinished setup is the row somebody is least likely to want and the
    /// one most likely to be pressed by accident on the way past.
    #[default]
    ReadyFirst,
    /// Straight down the alphabet, finished or not.
    NameAscending,
    NameDescending,
}

/// Every order the column offers, in the one place that decides what its Sort
/// list looks like.
pub const SORTS: &[Sort] = &[Sort::ReadyFirst, Sort::NameAscending, Sort::NameDescending];

impl Sort {
    /// What the row that chooses it says. The Steam column's words, because
    /// they are the same orders — "installed first" is what having something
    /// to play is called everywhere else on this bar.
    pub fn label(self) -> &'static str {
        match self {
            Sort::ReadyFirst => crate::i18n::text("shell-installed-first"),
            Sort::NameAscending => crate::i18n::text("shell-name-a-to-z"),
            Sort::NameDescending => crate::i18n::text("shell-name-z-to-a"),
        }
    }

    /// What it is called in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Sort::ReadyFirst => "installed-first",
            Sort::NameAscending => "name",
            Sort::NameDescending => "name-reversed",
        }
    }

    /// The order of that name, or `None` for a file naming one this column
    /// does not offer. Then the column comes up in the order it always has,
    /// and the file is left exactly as it was found.
    pub fn from_key(key: &str) -> Option<Sort> {
        SORTS.iter().copied().find(|sort| sort.key() == key)
    }

    /// Where one game stands against another under this order.
    fn compare(self, one: &GameRow, two: &GameRow) -> std::cmp::Ordering {
        let name = |row: &GameRow| row.name.to_lowercase();
        match self {
            Sort::ReadyFirst => {
                let ready = |row: &GameRow| matches!(row.state, GameState::Ready);
                ready(two)
                    .cmp(&ready(one))
                    .then_with(|| name(one).cmp(&name(two)))
            }
            Sort::NameAscending => name(one).cmp(&name(two)),
            Sort::NameDescending => name(two).cmp(&name(one)),
        }
    }
}

/// Where new games are kept, as the helper last said, or `None` before it
/// has said anything.
pub fn folder() -> Option<PathBuf> {
    ROOTS.lock().ok()?.as_deref().map(PathBuf::from)
}

/// The row that says where new games and apps are kept, and opens a walk to
/// change it.
///
/// Built here rather than in `settings` for the reason the PlayStation 3's
/// is built in `ps3`: it is one setting and one row, and the module that
/// owns the setting is the one that knows what it says.
pub fn folder_row() -> Entry {
    let comment = match folder() {
        Some(at) => crate::screenshot::abbreviated(&at),
        None => crate::i18n::text("windows-games-and-apps-folder-note").to_string(),
    };
    Entry::Folder(apps::Folder {
        title_message: Some("windows-games-and-apps-folder"),
        comment_message: None,
        identity: Some("windows:games-folder".to_string()),
        title: crate::i18n::text("windows-games-and-apps-folder").to_string(),
        comment: Some(comment),
        icon: Some(crate::icons::FILE_FOLDER.to_string()),
        entries: Vec::new(),
        place: Some(crate::files::Place::Volumes(crate::files::Shows::Folders(
            crate::settings::Picking::WindowsFolder,
        ))),
        chosen: false,
        over_the_list: false,
        person: None,
        portrait: None,
        used: None,
    })
}

/// A row with the tick on it, where it is the one in force.
fn tick(row: crate::menu::Entry, chosen: bool) -> crate::menu::Entry {
    match chosen {
        true => row.glyph(crate::icons::CHOSEN),
        false => row,
    }
}

// --- asking the helper ------------------------------------------------------

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

/// Everything the shell asks of the helper. One variant per subcommand.
///
/// Whole rather than only the parts the shell presses today: the surface the
/// two halves agree on is the helper's command list, and declaring half of it
/// would mean working out the other half again later from the other side of
/// a pipe. The variants nothing sends yet are the ones the file picker and
/// the game menu will send.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// What this machine can run a Windows game with, and where games live.
    Probe,
    /// Every Proton, for the Compatibility list.
    Tools,
    /// The games on the disk.
    Games,
    /// How to start one — the whole game, or something inside its prefix.
    Start {
        id: String,
        inside: Option<PathBuf>,
        compatibility: Option<String>,
        /// The display this is being started on, in pixels.
        ///
        /// What a setup's Windows desktop is made the size of, so its one
        /// fixed-size window opens in the middle of the screen. The helper
        /// cannot know it: the shell is the half of this that has displays.
        /// Ignored for the game itself — see `lxb-proton`'s `launch`.
        desktop: Option<(u32, u32)>,
    },
    /// A prefix of its own for this executable, and how to run it.
    New {
        exe: PathBuf,
        name: Option<String>,
        /// The display the setup is being started on. See [`Ask::Start`].
        desktop: Option<(u32, u32)>,
    },
    /// A prefix of its own with a game somebody already has copied into it.
    Import { dir: PathBuf, name: Option<String> },
    /// What inside this game's prefix could be the game.
    FoundIn { id: String },
    /// Something that ran in this prefix has ended: settle what the game
    /// plays against what is in there now.
    ///
    /// The one ask about a prefix the shell has no opinion about the answer
    /// to. A program that put another program in there was a setup, and the
    /// helper promotes what it installed and deletes the copy of the setup; a
    /// program that put nothing there was the program itself. Either way the
    /// answer is the one row, and the shell draws it.
    Settled { id: String },
    /// That one is the game.
    Target {
        id: String,
        at: String,
        name: Option<String>,
    },
    /// What a machine with no usable Proton would have to fetch.
    Offer,
    /// Fetch it.
    Fetch,
    /// Keep new games here from now on.
    Folder { dir: PathBuf },
    /// Leave nothing of this game's Windows running.
    End { id: String },
    /// Take a game off the disk, prefix and all.
    Remove { id: String },
    /// Run this game under this Proton from now on. `None` goes back to
    /// whatever the machine would choose.
    Under { id: String, proton: Option<String> },
}

/// A display written the way the helper's `--desktop` reads one.
fn a_display((across, down): (u32, u32)) -> String {
    format!("{across}x{down}")
}

/// One answer.
#[derive(Debug)]
pub enum Heard {
    Probed(Probe),
    Tooled(Tools),
    Listed(Games),
    Starting(String, Start),
    MadeOne(Made),
    FoundSome(Found),
    Offered(Offer),
    /// One line of a fetch or an import, tagged with the run it belongs to so
    /// a cancelled one cannot paint over the next.
    Working(u64, Progress),
    Removed(Removal),
    /// What was left of a game's Windows, and is not left any more.
    Ended(Ended),
    /// The helper is not answering, or is a version this shell cannot read.
    Broken(String),
}

/// The command line one ask is made with: the subcommand, and whatever that
/// subcommand needs after it.
///
/// Free-standing because it is the half of [`answer`] worth checking on its
/// own. Everything else there is a pipe and a process; this is the sentence
/// the two programs agree on, and a word wrong in it is a press that quietly
/// does something else — or, for `--desktop`, an installer back in the corner
/// of the screen with nothing to say why.
fn arguments(ask: &Ask) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    match ask {
        Ask::Probe => argv.push("probe".into()),
        Ask::Tools => argv.push("tools".into()),
        Ask::Games => argv.push("games".into()),
        Ask::Offer => argv.push("offer".into()),
        Ask::Fetch => argv.push("fetch".into()),
        Ask::Folder { dir } => {
            argv.push("folder".into());
            argv.push(dir.into());
        }
        Ask::End { id } => {
            argv.push("end".into());
            argv.push(id.into());
        }
        Ask::Remove { id } => {
            argv.push("remove".into());
            argv.push(id.into());
        }
        Ask::Under { id, proton } => {
            argv.push("use".into());
            argv.push(id.into());
            if let Some(name) = proton {
                argv.push("--proton".into());
                argv.push(name.into());
            }
        }
        Ask::FoundIn { id } => {
            argv.push("found".into());
            argv.push(id.into());
        }
        Ask::Settled { id } => {
            argv.push("settled".into());
            argv.push(id.into());
        }
        Ask::Start {
            id,
            inside,
            compatibility,
            desktop,
        } => {
            argv.push("start".into());
            argv.push(id.into());
            if let Some(exe) = inside {
                argv.push("--in".into());
                argv.push(exe.into());
            }
            if let Some(name) = compatibility {
                argv.push("--compatibility".into());
                argv.push(name.into());
            }
            if let Some(size) = desktop {
                argv.push("--desktop".into());
                argv.push(a_display(*size).into());
            }
        }
        Ask::New { exe, name, desktop } => {
            argv.push("new".into());
            argv.push(exe.into());
            if let Some(name) = name {
                argv.push("--name".into());
                argv.push(name.into());
            }
            if let Some(size) = desktop {
                argv.push("--desktop".into());
                argv.push(a_display(*size).into());
            }
        }
        Ask::Import { dir, name } => {
            argv.push("import".into());
            argv.push(dir.into());
            if let Some(name) = name {
                argv.push("--name".into());
                argv.push(name.into());
            }
        }
        Ask::Target { id, at, name } => {
            argv.push("target".into());
            argv.push(id.into());
            argv.push(at.into());
            if let Some(name) = name {
                argv.push("--name".into());
                argv.push(name.into());
            }
        }
    }
    argv
}

/// Whether anybody is waiting on this ask, and so whether a helper that
/// cannot answer it is worth putting on the screen.
///
/// **Almost everything here is a press**, and a press that goes nowhere has
/// to say so. The two asks that tidy up after a launch are not: by the time
/// either is asked the application is already gone, there is nothing on
/// screen it belongs to, and a sentence about it would arrive over the start
/// screen attached to nothing the person did.
///
/// They are also the asks an *older* helper will not know, which is no
/// coincidence: both were added after the first release of this pair. The two
/// halves of this integration are separate packages — see this module's head
/// — so a shell that has been updated can be talking to a helper that has
/// not, and `clap` answers a verb it has never heard of by printing nothing
/// at all. Without this, every Windows application that closed on such a
/// machine put *something went wrong* on the screen behind it.
fn anybody_is_waiting(ask: &Ask) -> bool {
    !matches!(ask, Ask::End { .. } | Ask::Settled { .. })
}

fn answer(helper: &Path, ask: &Ask, generation: u64, back: &Sender<Heard>) {
    let mut command = Command::new(helper);
    command.args(arguments(ask));

    let mut child = match command.stdin(Stdio::null()).stdout(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(%err, "the Windows Apps helper could not be run");
            if anybody_is_waiting(ask) {
                let _ = back.send(Heard::Broken(err.to_string()));
            }
            return;
        }
    };
    let mut said = false;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let heard = match ask {
                Ask::Probe => serde_json::from_str(line).map(Heard::Probed),
                Ask::Tools => serde_json::from_str(line).map(Heard::Tooled),
                // `target` and `use` answer with the one row they changed,
                // in the same shape `games` uses, so the shell has nothing
                // extra to learn.
                Ask::Games | Ask::Target { .. } | Ask::Under { .. } | Ask::Settled { .. } => {
                    serde_json::from_str(line).map(Heard::Listed)
                }
                Ask::Offer => serde_json::from_str(line).map(Heard::Offered),
                Ask::FoundIn { .. } => serde_json::from_str(line).map(Heard::FoundSome),
                Ask::Remove { .. } => serde_json::from_str(line).map(Heard::Removed),
                Ask::End { .. } => serde_json::from_str(line).map(Heard::Ended),
                Ask::Start { id, .. } => {
                    serde_json::from_str(line).map(|said| Heard::Starting(id.clone(), said))
                }
                Ask::New { .. } => serde_json::from_str(line).map(Heard::MadeOne),
                // `folder` answers with a probe: where new games go is one of
                // the things a probe says.
                Ask::Folder { .. } => serde_json::from_str(line).map(Heard::Probed),
                Ask::Fetch | Ask::Import { .. } => {
                    serde_json::from_str(line).map(|said| Heard::Working(generation, said))
                }
            };
            match heard {
                Ok(heard) => {
                    said = true;
                    if back.send(heard).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, line, "the Windows Apps helper said something unreadable")
                }
            }
        }
    }
    let status = child.wait();
    if !said {
        let why = format!("{HELPER} answered nothing ({status:?})");
        tracing::warn!(why, "the Windows Apps helper");
        if anybody_is_waiting(ask) {
            let _ = back.send(Heard::Broken(why));
        }
    }
}

// --- the shell's own state --------------------------------------------------

/// Windows Apps as one field of the shell.
pub struct Proton {
    /// `None` on a machine without the package: every method then answers as
    /// though this file did not exist.
    inner: Option<Inner>,
}

struct Inner {
    asks: Sender<Ask>,
    heard: Receiver<Heard>,
    /// The child of a fetch or an import while one runs, so it can be ended.
    working: Arc<Mutex<Option<std::process::Child>>>,
    /// Which run of a long job the lines arriving belong to. Bumped when one
    /// is started or abandoned, so a cancelled job's last lines cannot paint
    /// over the next one's first.
    ///
    /// **Shared with the worker, and that is the whole point.** This was two
    /// counters, one here and one in the thread, each starting at zero and
    /// counted on differently — the thread counted every job and this one
    /// only cancellations. So they disagreed from the very first import, and
    /// every line it sent was thrown away as belonging to a job nobody had
    /// asked for: no progress bar while a game was copied in, and no `done`,
    /// which is the line that reads the column again. A Notepad++ folder
    /// imported on this machine landed complete on the disk and left a row
    /// that said *Nothing has been installed here yet* until the shell was
    /// restarted.
    generation: Arc<AtomicU64>,
    /// What the helper last said. `None` until the first answer arrives,
    /// which is what the row's "looking" line is drawn from.
    probe: Option<Probe>,
    tools: Vec<Tool>,
    chosen: Option<String>,
    games: Vec<GameRow>,
    /// How far a fetch or an import has got, while one is running.
    doing: Option<Doing>,
    /// The helper is a version this shell cannot read. Nothing is drawn from
    /// it after that, and the row says so once.
    broken: bool,
    /// The order the column is listed in, as the settings file last had it.
    sort: Sort,
}

/// A long job, while it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Doing {
    pub stage: Stage,
    pub progress: Option<f32>,
    /// What it is about, where it is about one game.
    pub about: Option<String>,
}

/// What changed since the last frame. The shell reads this and reacts; there
/// are no callbacks.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Change {
    /// The rows have moved and the column must be built again.
    pub rows: bool,
    /// Something the person asked for has finished, one way or the other.
    pub finished: Option<Finished>,
    /// A game is ready to be started, with the command to start it.
    pub start: Option<Starting>,
    /// The executables inside a prefix, for the panel that confirms one.
    pub candidates: Option<(String, Vec<Candidate>)>,
    /// What a machine with no Proton would have to fetch.
    pub offer: Option<Offer>,
    /// The Protons on this machine have arrived, for a Compatibility list
    /// that is on screen saying it is still asking.
    pub tools: bool,
    /// How many launches the helper answered this poll, however it answered
    /// them — a command to run, or a reason it cannot be run.
    ///
    /// The shell counts the launches it has asked for and not yet forked, and
    /// this is what brings that count back down. A launch in that state is
    /// invisible everywhere else: the game is not running yet, so nothing
    /// that looks at what *is* running can see that one is on its way. See
    /// `Shell::a_windows_launch_ended`, where mistaking it for nothing at all
    /// would empty the Windows a launch is about to start in.
    pub launches_answered: usize,
}

/// A game, and everything needed to start it. The shell starts it — see the
/// module's own note on why the helper never does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Starting {
    pub id: String,
    /// The whole command, program first.
    pub argv: Vec<String>,
    /// What to add to the environment. Nothing is removed.
    pub environment: Vec<(String, String)>,
    /// Where to run it — the executable's own folder.
    pub directory: String,
}

/// One thing the person asked for, and how it went.
#[derive(Debug, Clone, PartialEq)]
pub enum Finished {
    /// A Proton and its runtime are here now.
    Fetched,
    /// A game was made, and this is its identifier.
    Made(String),
    /// A game is gone.
    Removed(String),
    /// It did not happen. The sentence is already in the person's language.
    Failed(String),
}

/// The readers the column, its menus and its Settings page are built from.
impl Proton {
    /// A machine without the package.
    pub fn absent() -> Proton {
        Proton { inner: None }
    }

    /// One of these with the line the worker speaks on held open, so a test
    /// can *be* the worker.
    ///
    /// No thread and no process: the question these answer is what [`poll`]
    /// does with a line, and which run that line belongs to, and neither of
    /// those is a question about this machine. The counter comes back
    /// because it is what the worker reads to stamp a line with — see
    /// [`Inner::generation`].
    ///
    /// [`poll`]: Proton::poll
    #[cfg(test)]
    fn listening() -> (Proton, Sender<Heard>, Arc<AtomicU64>) {
        let (asks, _) = std::sync::mpsc::channel::<Ask>();
        let (says, heard) = std::sync::mpsc::channel::<Heard>();
        let generation = Arc::new(AtomicU64::new(0));
        let proton = Proton {
            inner: Some(Inner {
                asks,
                heard,
                working: Arc::new(Mutex::new(None)),
                generation: Arc::clone(&generation),
                probe: None,
                tools: Vec::new(),
                chosen: None,
                games: Vec::new(),
                doing: None,
                broken: false,
                sort: Sort::default(),
            }),
        };
        (proton, says, generation)
    }

    /// One of these with no worker behind it, holding exactly what a test
    /// puts in it.
    ///
    /// The channels are made and dropped on the spot: nothing in `rows` or
    /// the menus sends anything, and a test that ends up here wanting a reply
    /// wants [`Proton::start`] and a helper.
    #[cfg(test)]
    fn holding(probe: Option<Probe>, tools: Vec<Tool>, games: Vec<GameRow>) -> Proton {
        let (asks, _) = std::sync::mpsc::channel::<Ask>();
        let (_, heard) = std::sync::mpsc::channel::<Heard>();
        Proton {
            inner: Some(Inner {
                asks,
                heard,
                working: Arc::new(Mutex::new(None)),
                generation: Arc::new(AtomicU64::new(0)),
                probe,
                tools,
                chosen: None,
                games,
                doing: None,
                broken: false,
                sort: Sort::default(),
            }),
        }
    }

    /// Start the worker and ask it what is here.
    pub fn start(helper: PathBuf) -> Proton {
        let (asks, to_do) = std::sync::mpsc::channel::<Ask>();
        let (says, heard) = std::sync::mpsc::channel::<Heard>();
        let working: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
        let generation = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&generation);
        std::thread::Builder::new()
            .name("lxb-proton".to_string())
            .spawn(move || {
                while let Ok(ask) = to_do.recv() {
                    // Read as the job starts, not counted here: the shell
                    // bumps it when it asks and when it gives up, and this
                    // thread's business is only to say which job a line
                    // came out of.
                    answer(&helper, &ask, counted.load(Ordering::SeqCst), &says);
                }
            })
            .ok();
        let _ = asks.send(Ask::Probe);
        let _ = asks.send(Ask::Games);
        // The Protons, before anybody asks for them. A Compatibility list
        // with nothing in it cannot be opened at all — `menu::Menu::descend`
        // refuses a list with no row anybody can stand on — so a shell that
        // waited until the press would have a Compatibility row that did
        // nothing the first time it was pressed and worked the second. This
        // reads two directories.
        let _ = asks.send(Ask::Tools);
        Proton {
            inner: Some(Inner {
                asks,
                heard,
                working,
                generation,
                probe: None,
                tools: Vec::new(),
                chosen: None,
                games: Vec::new(),
                doing: None,
                broken: false,
                sort: crate::settings::windows_sort().unwrap_or_default(),
            }),
        }
    }

    /// Whether this machine has the integration at all.
    ///
    /// Nothing asks it: every caller that wanted this question wanted the
    /// *pair* of it and the Integration switch, and asks
    /// [`crate::proton::offered`] and
    /// [`crate::settings::windows_integration`] instead. Kept because it is
    /// the handle's own honest answer about itself, and because a reader that
    /// says what a handle holds costs nothing to keep.
    #[allow(dead_code)]
    pub fn here(&self) -> bool {
        self.inner.is_some()
    }

    pub fn send(&self, ask: Ask) {
        let Some(inner) = &self.inner else {
            return;
        };
        // A fetch or an import is the only kind of ask that answers in more
        // than one line, and so the only kind whose lines can still be
        // arriving when somebody has moved on. Counted here, where the job
        // is *asked for*, so the worker and this side can never be counting
        // different things — see [`Inner::generation`].
        if matches!(ask, Ask::Fetch | Ask::Import { .. }) {
            inner.generation.fetch_add(1, Ordering::SeqCst);
        }
        let _ = inner.asks.send(ask);
    }

    /// Ask for the games again — after a game has been installed or removed
    /// by anything other than this shell.
    pub fn reread(&self) {
        self.send(Ask::Games);
    }

    /// Whether a Proton that can actually start a game is here.
    pub fn ready(&self) -> bool {
        self.inner
            .as_ref()
            .and_then(|inner| inner.probe.as_ref())
            .and_then(|probe| probe.proton.as_ref())
            .is_some_and(Tool::usable)
    }

    /// Whether Valve's client is installed, which is the one thing that
    /// decides whether a game is offered a Compatibility choice.
    pub fn steam_here(&self) -> bool {
        self.inner
            .as_ref()
            .and_then(|inner| inner.probe.as_ref())
            .is_some_and(|probe| probe.steam)
    }

    /// Every Proton, for the Compatibility list.
    pub fn tools(&self) -> &[Tool] {
        self.inner.as_ref().map_or(&[], |inner| &inner.tools)
    }

    /// The one a game with no choice of its own runs under.
    ///
    /// Not asked yet: the Compatibility list marks a game's own choice and
    /// offers "This machine's choice" to give it back, which needs the list
    /// and not the name. Kept for the row that will one day say what that
    /// choice is.
    #[allow(dead_code)]
    pub fn default_tool(&self) -> Option<&str> {
        self.inner.as_ref()?.chosen.as_deref()
    }

    /// A long job, while one runs.
    pub fn doing(&self) -> Option<&Doing> {
        self.inner.as_ref()?.doing.as_ref()
    }

    /// The games, as the helper last listed them.
    pub fn games(&self) -> &[GameRow] {
        self.inner.as_ref().map_or(&[], |inner| &inner.games)
    }

    /// One game by its identifier.
    pub fn game(&self, id: &str) -> Option<&GameRow> {
        self.games().iter().find(|game| game.id == id)
    }

    /// The games in the order somebody reads them in.
    ///
    /// [`Proton::games`] is the helper's listing, which is the order the disk
    /// gave it; this is the order the column is in. Here rather than at each
    /// place that wants it, so a list of games offered anywhere on the bar
    /// stands in the same order as the column — see [`Proton::rows`], which
    /// is the other caller.
    pub fn games_in_order(&self) -> Vec<&GameRow> {
        let sort = self.sort();
        let mut listed: Vec<&GameRow> = self.games().iter().collect();
        listed.sort_by(|one, two| sort.compare(one, two));
        listed
    }

    /// The order the column is listed in.
    pub fn sort(&self) -> Sort {
        self.inner
            .as_ref()
            .map_or(Sort::default(), |inner| inner.sort)
    }

    /// List it in this order from now on. `true` where that is a change, so
    /// the caller knows whether the column has to be built again.
    pub fn sort_by(&mut self, sort: Sort) -> bool {
        let Some(inner) = &mut self.inner else {
            return false;
        };
        if inner.sort == sort {
            return false;
        }
        inner.sort = sort;
        crate::settings::remember_windows_sort(sort);
        true
    }

    /// Run one game under this Proton from now on, or, with no name, under
    /// whatever this machine would choose.
    pub fn run_under(&self, id: &str, proton: Option<&str>) {
        tracing::info!(id, ?proton, "a Windows game is to run with another Proton");
        self.send(Ask::Under {
            id: id.to_string(),
            proton: proton.map(str::to_string),
        });
    }

    /// The rows under a game's Compatibility: the machine's own choice first
    /// — the row that gives a game its default back, as "Steam's choice" does
    /// on a Steam game — then every Proton this machine has, the one in force
    /// ticked. A row saying so while the helper has not been asked yet.
    ///
    /// Asking is done here rather than when the menu opens, because the list
    /// is only ever read while this menu is on screen and a machine's Protons
    /// change when somebody installs one.
    pub fn compatibility_rows(&self, id: &str) -> Vec<crate::menu::Entry> {
        use crate::menu::{Command, Entry};
        let chosen = self.game(id).and_then(|game| game.proton.clone());
        let mut rows = Vec::new();
        if self.tools().is_empty() {
            rows.push(
                Entry::new(
                    Command::Dismiss,
                    crate::i18n::text("windows-asking-what-is-here"),
                )
                .reading(),
            );
            return rows;
        }
        // Short, and deliberately without the Proton it resolves to on it.
        // The name would be the useful half and it is the half a context
        // menu row has no room for: this list is drawn at a fixed width and
        // "This machine's choice (Proton 11.0)" came back as "This machine's
        // cho…", which is a row that costs a line and says nothing. Steam's
        // own row is the same three words for the same reason.
        rows.push(tick(
            Entry::new(
                Command::WindowsUnder(None),
                crate::i18n::text("windows-this-machines-choice").to_string(),
            ),
            chosen.is_none(),
        ));
        for tool in self.tools() {
            let row = Entry::new(
                Command::WindowsUnder(Some(crate::settings::intern(&tool.name))),
                tool.name.clone(),
            );
            // A Proton whose runtime is not on this machine cannot start
            // anything, and saying so with an outline is the difference
            // between a row that does nothing and a shell that looks broken.
            let row = match tool.usable() {
                true => row,
                false => row.reading(),
            };
            rows.push(tick(row, chosen.as_deref() == Some(tool.name.as_str())));
        }
        rows.push(Entry::new(Command::Dismiss, crate::i18n::text("shell-cancel")).group(1));
        rows
    }

    /// End a fetch or an import that is running, and ignore whatever it says
    /// on its way out.
    pub fn stop_working(&mut self) {
        let Some(inner) = &mut self.inner else {
            return;
        };
        inner.generation.fetch_add(1, Ordering::SeqCst);
        inner.doing = None;
        if let Ok(mut held) = inner.working.lock() {
            if let Some(mut child) = held.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// Everything the helper has said since the last frame.
    pub fn poll(&mut self) -> Change {
        let mut change = Change::default();
        let Some(inner) = &mut self.inner else {
            return change;
        };
        let mut asks: Vec<Ask> = Vec::new();
        while let Ok(heard) = inner.heard.try_recv() {
            // One guard, before anything is read out of a record: a helper
            // whose protocol this shell does not know is a helper nothing is
            // drawn from. The person is told once, in words, and never a
            // number.
            if let Some(protocol) = protocol_of(&heard) {
                if protocol != PROTOCOL {
                    if !inner.broken {
                        tracing::warn!(
                            said = protocol,
                            expected = PROTOCOL,
                            "the Windows Apps helper speaks another protocol"
                        );
                        inner.broken = true;
                        change.rows = true;
                    }
                    continue;
                }
            }
            match heard {
                Heard::Probed(probe) => {
                    if let Ok(mut roots) = ROOTS.lock() {
                        *roots = probe.prefix_roots.first().cloned();
                    }
                    inner.probe = Some(probe);
                    change.rows = true;
                }
                Heard::Tooled(tools) => {
                    inner.chosen = tools.chosen.clone();
                    inner.tools = tools.tools;
                    change.tools = true;
                }
                Heard::Listed(games) => {
                    // `target` answers with one row; `games` with all of
                    // them. One row is an update of that row, not a library
                    // of one.
                    if games.games.len() == 1 && inner.games.len() > 1 {
                        let one = games.games.into_iter().next().expect("just measured");
                        if let Some(had) = inner.games.iter_mut().find(|had| had.id == one.id) {
                            *had = one;
                        }
                    } else {
                        inner.games = games.games;
                    }
                    change.rows = true;
                }
                Heard::Starting(id, start) => {
                    // Answered either way, which is the half this counts: a
                    // launch that is refused is a launch no longer on its way.
                    change.launches_answered += 1;
                    match start.trouble {
                        Some(trouble) => {
                            change.finished =
                                Some(Finished::Failed(trouble_sentence(trouble).to_string()))
                        }
                        None => {
                            change.start = Some(Starting {
                                id,
                                argv: start.argv,
                                environment: start.environment,
                                directory: start.directory,
                            })
                        }
                    }
                }
                Heard::MadeOne(made) => match made.trouble {
                    Some(trouble) => {
                        change.finished =
                            Some(Finished::Failed(trouble_sentence(trouble).to_string()))
                    }
                    None => {
                        if let Some(game) = made.game {
                            let id = game.id.clone();
                            inner.games.push(game);
                            change.rows = true;
                            change.finished = Some(Finished::Made(id.clone()));
                            if let Some(start) = made.start {
                                change.start = Some(Starting {
                                    id,
                                    argv: start.argv,
                                    environment: start.environment,
                                    directory: start.directory,
                                });
                            }
                        }
                    }
                },
                Heard::FoundSome(found) => change.candidates = Some((found.id, found.candidates)),
                Heard::Offered(offer) => change.offer = Some(offer),
                Heard::Working(generation, progress) => {
                    // A line from a job somebody cancelled is a line about
                    // nothing.
                    if generation != inner.generation.load(Ordering::SeqCst) {
                        continue;
                    }
                    match progress.stage {
                        Stage::Done => {
                            inner.doing = None;
                            change.finished = Some(Finished::Fetched);
                            asks.push(Ask::Probe);
                            asks.push(Ask::Games);
                        }
                        Stage::Failed => {
                            inner.doing = None;
                            change.finished = Some(Finished::Failed(
                                trouble_sentence(progress.trouble.unwrap_or(Trouble::Other))
                                    .to_string(),
                            ));
                        }
                        stage => {
                            inner.doing = Some(Doing {
                                stage,
                                progress: progress.progress,
                                about: None,
                            });
                        }
                    }
                    change.rows = true;
                }
                // Nothing on the bar, and a line in the log. A count above
                // zero here is a Windows that outlived the screen it was on,
                // which is worth being able to look up afterwards.
                Heard::Ended(gone) if gone.left > 0 => tracing::warn!(
                    id = gone.id,
                    found = gone.found,
                    left = gone.left,
                    "some of a game's Windows could not be ended"
                ),
                Heard::Ended(gone) => match gone.found {
                    0 => tracing::debug!(id = gone.id, "nothing of that game was still running"),
                    found => tracing::info!(
                        id = gone.id,
                        found,
                        killed = gone.killed,
                        "ended what was left of a game's Windows"
                    ),
                },
                Heard::Removed(removal) => {
                    if removal.removed {
                        inner.games.retain(|game| game.id != removal.id);
                        change.rows = true;
                        change.finished = Some(Finished::Removed(removal.id));
                    } else {
                        change.finished = Some(Finished::Failed(
                            trouble_sentence(removal.trouble.unwrap_or(Trouble::Other)).to_string(),
                        ));
                    }
                }
                Heard::Broken(why) => {
                    tracing::warn!(why, "the Windows Apps helper");
                    // A helper that cannot answer cannot be about to hand
                    // over a launch either, and a count left standing would
                    // hold off every tidy-up for the rest of the session.
                    change.launches_answered = change.launches_answered.saturating_add(1);
                    change.finished = Some(Finished::Failed(
                        crate::i18n::text("windows-trouble-other").to_string(),
                    ));
                }
            }
        }
        for ask in asks {
            let _ = inner.asks.send(ask);
        }
        change
    }
}

/// The protocol a record declares, for the one guard every answer passes.
fn protocol_of(heard: &Heard) -> Option<u32> {
    match heard {
        Heard::Probed(said) => Some(said.protocol),
        Heard::Tooled(said) => Some(said.protocol),
        Heard::Listed(said) => Some(said.protocol),
        Heard::Starting(_, said) => Some(said.protocol),
        Heard::MadeOne(said) => Some(said.protocol),
        Heard::FoundSome(said) => Some(said.protocol),
        Heard::Offered(said) => Some(said.protocol),
        Heard::Working(_, said) => Some(said.protocol),
        Heard::Removed(said) => Some(said.protocol),
        Heard::Ended(said) => Some(said.protocol),
        Heard::Broken(_) => None,
    }
}

/// End anything still running when the shell goes.
impl Drop for Proton {
    fn drop(&mut self) {
        self.stop_working();
    }
}

// --- what the shell draws ---------------------------------------------------

impl Proton {
    /// The line under the Windows Apps row in the Games column, or `None`
    /// where there is no row at all.
    ///
    /// `None` is how the row is taken off: a machine without the package has
    /// no Windows Apps row, and neither has one whose helper this shell
    /// cannot read.
    pub fn note(&self) -> Option<String> {
        let inner = self.inner.as_ref()?;
        if inner.broken {
            return Some(crate::i18n::text("shell-not-working-on-this-machine").to_string());
        }
        let Some(probe) = &inner.probe else {
            return Some(crate::i18n::text("windows-looking").to_string());
        };
        if let Some(doing) = &inner.doing {
            return Some(
                crate::i18n::text(match doing.stage {
                    Stage::Checking | Stage::Settling => "windows-settling",
                    _ => "windows-getting-ready",
                })
                .to_string(),
            );
        }
        if probe.proton.as_ref().is_none_or(|tool| !tool.usable()) {
            return Some(crate::i18n::text("windows-press-to-set-up").to_string());
        }
        Some(match inner.games.len() {
            0 => crate::i18n::text("windows-no-games-yet").to_string(),
            count => crate::message!("windows-count-games", "count" => count),
        })
    }

    /// The bar under that row, while something is coming down.
    pub fn arriving(&self) -> Option<f32> {
        self.inner.as_ref()?.doing.as_ref()?.progress
    }

    /// The Windows Apps column: the two rows that put a program into a
    /// prefix, and then the games.
    ///
    /// Always at least the two rows, which is the whole difference between
    /// this column and the other integrations': they are how the first game
    /// is installed, so a column that waited for a game could never gain one.
    /// Empty only where there is no package — see [`apps::shelve_windows`],
    /// which reads an empty list as "the integration has gone".
    ///
    /// **Two and not three.** There was a third — *Run something inside a
    /// game*, which opened a list of games and then a walk of the one
    /// chosen. It is a thing somebody does once a year, to run a patcher or
    /// a settings program, and it stood at the head of the column above
    /// every game, every day, in front of the thing people actually came
    /// for. It is on a game's own menu now, where its subject already is —
    /// which also takes the first of its two steps away, since the game is
    /// the one the menu is open over.
    pub fn rows(&self, running_inside: Option<&str>) -> Vec<Entry> {
        let Some(inner) = &self.inner else {
            return Vec::new();
        };
        if inner.broken {
            return Vec::new();
        }
        let ready = self.ready();
        // Whose walk this is, where a game's **Run something inside** opened
        // it rather than the column itself. The two walks are the same rows
        // off the same disk and only this row says them apart, so while one
        // is open it says the game by name: somebody standing in front of
        // their own Downloads has to be able to see where what they pick is
        // going to land. See [`crate::Shell::open_windows_run_inside_picker`].
        let inside = running_inside
            .and_then(|id| inner.games.iter().find(|game| game.id == id))
            .map(|game| game.name.clone());
        let mut rows = vec![
            // Not a row kind of its own: a row that opens a column *is* a
            // folder row here, and the shell's own file navigation is what
            // the plan asks to walk. All this one carries that no other does
            // is the walk it opens — the first in this shell whose answer is
            // a file. See `crate::files::Shows::Executable`.
            Entry::Folder(apps::Folder {
                title_message: match inside {
                    Some(_) => None,
                    None => Some("windows-run-an-exe"),
                },
                comment_message: None,
                identity: Some(RUN_A_PROGRAM.to_string()),
                title: match &inside {
                    Some(game) => crate::message!("windows-run-inside-game", "game" => game),
                    None => crate::i18n::text("windows-run-an-exe").to_string(),
                },
                comment: match inside.is_some() {
                    // The title says the whole of it — *run inside this
                    // game* — and a second line repeating it would be the
                    // row twice.
                    true => None,
                    false => Some(match ready {
                        true => crate::i18n::text("windows-run-an-exe-note").to_string(),
                        // The one press worth offering on a machine that has
                        // nothing to run anything with: it fetches first and
                        // installs after.
                        false => crate::i18n::text("windows-press-to-set-up").to_string(),
                    }),
                },
                // A plus: this row adds a program the machine did not have.
                // The Windows mark belongs to the row below it, which is
                // about a game that is already here.
                icon: Some(crate::icons::ADD.to_string()),
                entries: Vec::new(),
                place: Some(crate::files::Place::Volumes(
                    crate::files::Shows::Executable,
                )),
                chosen: false,
                over_the_list: true,
                person: None,
                portrait: None,
                used: None,
            }),
            // The way in for a game that came without an installer: a folder
            // copied off another machine, or one an archive was unpacked
            // into. A *folder* walk and not the executable walk the first row
            // opens, because an executable without the files beside it is a
            // game that starts and stops — so the whole folder is copied in.
            Entry::Folder(apps::Folder {
                title_message: Some("windows-add-a-game-or-app-folder"),
                comment_message: None,
                identity: Some("windows:folder".to_string()),
                title: crate::i18n::text("windows-add-a-game-or-app-folder").to_string(),
                comment: Some(match ready {
                    true => crate::i18n::text("windows-add-a-game-or-app-folder-note").to_string(),
                    false => crate::i18n::text("windows-press-to-set-up").to_string(),
                }),
                icon: Some(crate::icons::FILE_FOLDER.to_string()),
                place: Some(crate::files::Place::Volumes(crate::files::Shows::Folders(
                    crate::settings::Picking::WindowsGameFolder,
                ))),
                entries: Vec::new(),
                chosen: false,
                over_the_list: true,
                person: None,
                portrait: None,
                used: None,
            }),
        ];
        // The order the column is listed in is applied here rather than on
        // the way in from the helper: the helper lists what is on the disk,
        // and what order somebody reads it in is the shell's business.
        rows.extend(self.games_in_order().into_iter().map(|game| {
            Entry::WindowsGame(apps::WindowsGame {
                id: game.id.clone(),
                name: game.name.clone(),
                note: match game.state {
                    GameState::Ready => crate::i18n::text("windows-ready").to_string(),
                    GameState::Unfinished if game.prefix_built => {
                        crate::i18n::text("windows-install-unfinished").to_string()
                    }
                    GameState::Unfinished => {
                        crate::i18n::text("windows-nothing-installed-yet").to_string()
                    }
                },
                progress: None,
                ready: matches!(game.state, GameState::Ready),
                at: PathBuf::from(&game.at),
                proton: game.proton.clone(),
                cover: game.cover.clone(),
            })
        }));
        rows
    }
}

/// What the row that walks the disk for a `.exe` is called in the tree.
///
/// Named because two things reach for it: the column draws it, and a game's
/// **Run something inside** opens the very same walk from that game's menu
/// — see [`crate::Shell::open_windows_run_inside_picker`].
pub const RUN_A_PROGRAM: &str = "windows:run";

/// What a press on the Windows Apps row does, asked at the press because
/// every answer is a fact about a helper and a disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// The helper has not answered yet. Ask it again rather than doing
    /// nothing: a row that does nothing twice is a row somebody stops
    /// pressing.
    Waiting,
    /// Something is already being downloaded. Say so and leave it alone.
    Working,
    /// There is nothing on this machine that can run a Windows program.
    /// Offer to fetch one.
    SetUp,
    /// Into the column.
    Enter,
}

impl Proton {
    pub fn press(&self) -> Press {
        let Some(inner) = &self.inner else {
            return Press::Waiting;
        };
        if inner.doing.is_some() {
            return Press::Working;
        }
        if inner.probe.is_none() {
            return Press::Waiting;
        }
        match self.ready() {
            true => Press::Enter,
            false => Press::SetUp,
        }
    }
}
