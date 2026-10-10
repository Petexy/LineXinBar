//! One Windows game: the directory that is its whole installation, and the
//! small file that says what it is.
//!
//! ```text
//! <root>/<game-id>/
//!     lxb.json       what it is called, what starts it, what it runs under
//!     pfx/           the Wine prefix: drive_c, the registry, its own saves
//!     version        Proton's, and the only honest "this is finished"
//!     ...            config_info, tracked_files, pfx.lock — Proton's
//! ```
//!
//! The enclosing directory is what Proton is handed as its compatibility
//! data path, and `pfx/` is the Wine prefix inside it. Everything but
//! `lxb.json` in there belongs to Proton.
//!
//! ## Why the whole game lives inside the prefix
//!
//! Because a Windows game does not distinguish between itself and its
//! installation. An installer writes files, registry keys, a start-menu
//! entry and a save directory, and which of those are "the game" is not a
//! question with an answer. So the unit this shell owns, backs up and
//! deletes is the whole directory — which is also why Uninstall says, in
//! words, that the saves go with it.
//!
//! ## Finished, half-finished, and not a game at all
//!
//! A prefix that Proton built and a prefix an installer died in the middle of
//! look nearly alike, so [`Game::state`] is read rather than assumed:
//! Proton's own `version` file is written last and is what it reads back
//! itself, so its presence is the one honest marker that the prefix is whole.
//! A half-finished one is still listed — it is somebody's interrupted
//! install, and **Run .exe inside of...** is how it is rescued.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What this helper keeps about one game, beside Proton's own files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// What it is called on the bar.
    pub name: String,
    /// The executable a press of Play runs.
    ///
    /// Relative to the game's own directory for a game that lives in its
    /// prefix — an installation, or a folder copied in — so that moving the
    /// games folder to another disk does not break every shortcut in it.
    ///
    /// **Absolute for a program this shell never copied anywhere**: a `.exe`
    /// pressed in Files is run where the person keeps it, with everything it
    /// came with still beside it, so what plays it is a path on their own
    /// disk. See `install`'s head for why nothing is copied, and
    /// [`Game::runs_in_place`], which is this fact asked as a question.
    pub play: Option<String>,
    /// What to put after it, if anything.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// The Proton the user chose for this game, by name. `None` means
    /// whatever the machine would choose today, which is what somebody who
    /// has never opened Compatibility wants.
    ///
    /// Kept even while Steam is gone — see the plan: a machine that loses
    /// Steam runs the game under the managed build and remembers the choice
    /// for Steam's return.
    pub proton: Option<String>,
    /// The executable the setup started from, for the log and for saying
    /// what an interrupted install was installing.
    pub came_from: Option<String>,
    /// The disc it was set up from, where it was one — what a disc in the
    /// drive is matched against to find its own Windows, and the game it
    /// installed. See [`crate::report::Disc`] and [`Game::the_discs_own`].
    ///
    /// Left out of the file for a game that came off none, so an `lxb.json`
    /// written before discs reads back the same and a newer one says nothing
    /// it does not have to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc: Option<crate::report::Disc>,
}

/// One game on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    /// The directory name, which is the identifier. The shell knows this
    /// game as `windows:<id>`.
    pub id: String,
    /// Its own directory — the compatibility data path.
    pub at: PathBuf,
    pub kept: Kept,
}

/// How far along a game's directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Something has been named to play.
    Ready,
    /// Ours, and nothing has been named to play yet — an install that was
    /// interrupted, or one whose executable has not been confirmed. Listed,
    /// and recoverable through **Run .exe inside of...**.
    Unfinished,
}

impl Game {
    /// Whether there is something to play.
    ///
    /// One question, and deliberately not two. Proton's own `version` file
    /// says the *prefix* has been built, which is a different fact and not
    /// this one: a game imported as a folder has no prefix until the first
    /// time it is played, and Proton builds one then. Asking for `version`
    /// here listed every imported game as unfinished — found by importing
    /// one and looking at the row.
    pub fn state(&self) -> State {
        match self.kept.play.is_some() {
            true => State::Ready,
            false => State::Unfinished,
        }
    }

    /// Whether Proton has built the prefix in here.
    ///
    /// `version` is written last and is what Proton reads back itself, so
    /// it is the honest marker — a run that died midway leaves `pfx/` and
    /// `tracked_files` without it. Used to tell "the installer never got
    /// anywhere" from "this has simply never been played".
    pub fn prefix_built(&self) -> bool {
        self.at.join("version").is_file()
    }

    /// The Wine prefix inside it.
    pub fn pfx(&self) -> PathBuf {
        self.at.join("pfx")
    }

    /// The executable a press of Play runs, where one has been named.
    pub fn target(&self) -> Option<PathBuf> {
        self.kept.play.as_ref().map(|play| self.at.join(play))
    }

    /// Whether what this plays is a program outside its own directory — one
    /// pressed in Files and run where the person keeps it.
    ///
    /// The one fact that says a launch ending in here is worth looking at the
    /// prefix over: a game that plays something *inside* its prefix is
    /// installed and settled, and one that plays something outside has not
    /// been told yet whether what it ran was a setup. Asked of the path
    /// rather than remembered in a field of its own, because it is the same
    /// fact written once — the moment what it plays moves inside the prefix,
    /// it has settled.
    pub fn runs_in_place(&self) -> bool {
        self.kept
            .play
            .as_deref()
            .is_some_and(|play| Path::new(play).is_absolute())
    }

    /// Whether this is a disc's own Windows: set up from that disc, and still
    /// playing the program on it because nothing has been installed.
    ///
    /// The shell shows no row for one — the disc's own row runs in it while
    /// the disc is in — and the next press on that disc runs here rather than
    /// in a new Windows, so a game played straight off its disc keeps its
    /// saves. Read from the two facts it is made of, as
    /// [`Game::runs_in_place`] is, so the moment a setup on that disc
    /// installs something this stops being true without anybody saying so.
    pub fn the_discs_own(&self, disc: &crate::report::Disc) -> bool {
        self.kept.disc.as_ref() == Some(disc) && self.runs_in_place()
    }

    /// Write `lxb.json` back, beside itself and then over itself, so a
    /// session that ends mid-write leaves the old one rather than half of
    /// the new one.
    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.at)?;
        let text = serde_json::to_string_pretty(&self.kept)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        let at = self.at.join(KEPT);
        let temp = self.at.join(format!("{KEPT}.lxb-{}", std::process::id()));
        std::fs::write(&temp, format!("{text}\n"))?;
        std::fs::rename(&temp, at).inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
    }
}

/// The file that makes a directory one of ours.
const KEPT: &str = "lxb.json";

/// Every game under every root, in the order the roots are given — so the
/// folder new games go in is listed first, and a game on an unplugged disk
/// is simply not there rather than being an error.
pub fn all(roots: &[PathBuf]) -> Vec<Game> {
    let mut games: Vec<Game> = Vec::new();
    for root in roots {
        let Ok(listing) = std::fs::read_dir(root) else {
            continue;
        };
        let mut here: Vec<Game> = listing
            .filter_map(Result::ok)
            .filter_map(|entry| read(&entry.path()))
            // A game id is the directory name, so two roots holding the
            // same name hold one game as far as the shell is concerned. The
            // first root wins, which is the one new games go in.
            .filter(|game| !games.iter().any(|had| had.id == game.id))
            .collect();
        here.sort_by(|a, b| a.id.cmp(&b.id));
        games.append(&mut here);
    }
    games
}

/// One game, read out of a directory, or `None` for a directory that is not
/// one of ours.
pub fn read(at: &Path) -> Option<Game> {
    let id = at.file_name()?.to_str()?.to_string();
    let text = std::fs::read_to_string(at.join(KEPT)).ok()?;
    let kept: Kept = serde_json::from_str(&text).ok()?;
    Some(Game {
        id,
        at: at.to_path_buf(),
        kept,
    })
}

/// Make a new game's directory under `root` and write its `lxb.json`.
///
/// The identifier is made from the name, so a directory somebody opens in a
/// file manager says what it holds. Where that name is taken — two games
/// genuinely called the same thing, or the same installer run twice — a
/// number is added rather than the existing one being touched.
pub fn make(root: &Path, name: &str, came_from: Option<&Path>) -> std::io::Result<Game> {
    std::fs::create_dir_all(root)?;
    let id = free_id(root, name);
    let at = root.join(&id);
    std::fs::create_dir_all(&at)?;
    let game = Game {
        id,
        at,
        kept: Kept {
            name: name.to_string(),
            play: None,
            arguments: Vec::new(),
            proton: None,
            came_from: came_from.map(|at| at.to_string_lossy().into_owned()),
            disc: None,
        },
    };
    game.save()?;
    Ok(game)
}

/// A directory name for this game that nothing under `root` is using.
fn free_id(root: &Path, name: &str) -> String {
    let base = slug(name);
    if !root.join(&base).exists() {
        return base;
    }
    // Two is where a second copy starts, because the first has no number.
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !root.join(id).exists())
        .unwrap_or(base)
}

/// A game's name as a directory name: lower case, letters and digits kept,
/// everything else a single hyphen.
///
/// Deliberately plain. This is a path somebody may type, back up, or name in
/// a bug report, and a Windows game is called things like
/// `FINAL FANTASY VII REMAKE INTERGRADE™` and `Portal 2 (Steam)`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    // Everything in there is now an ASCII letter, digit or hyphen, so a
    // byte is a character and the cut cannot land inside one. Sixty-four is
    // well under every filesystem's limit and still long enough to read.
    let kept = trimmed[..trimmed.len().min(64)].trim_end_matches('-');
    match kept {
        // A name that was entirely punctuation — and Windows games are
        // called things like `★`.
        "" => "windows-game".to_string(),
        kept => kept.to_string(),
    }
}

/// Every process of this game's that is still running: its Wine, and the
/// container the Wine runs inside.
///
/// **Two environment variables, because the family is in two halves.**
/// `WINEPREFIX` names everything inside the prefix — `wineserver` itself,
/// Wine's own services, and whatever somebody started — and
/// `STEAM_COMPAT_DATA_PATH` names the layers around them, which never hear of
/// a prefix: Valve's supervisor, `bwrap`, and the container's own adverb.
/// Measured on a real launch: the supervisor and `bwrap` carry only the
/// second, and `wineserver` carries both.
///
/// **`WINEPREFIX` is compared with its last slash taken off.** Wine writes it
/// with one and this helper writes it without, and the two spellings are the
/// same directory. Asked the strict way nothing ever matched, which is a
/// question that had been answering *nothing is running in there* since the
/// day it was written.
///
/// Asked of the processes rather than of `pfx.lock`: Proton takes that lock
/// only while it sets the prefix up and releases it *before* the game starts,
/// so a lock file says nothing about whether a game is running.
///
/// Reads `/proc`, which on Linux shows this user their own processes'
/// environments. A process this user may not read is one that is not theirs
/// and so is not their game.
pub fn users(game: &Game) -> Vec<i32> {
    let Ok(listing) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found: Vec<i32> = listing
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let environ = std::fs::read(entry.path().join("environ")).ok()?;
            names_this_game(&environ, game).then_some(pid)
        })
        .collect();
    found.sort_unstable();
    found
}

/// Whether one process's environment says it belongs to this game.
///
/// The whole of the matching, where it can be asked of bytes rather than of
/// the machine — `/proc/<pid>/environ` is the environment a process was
/// *started* with, which no test can arrange for itself without starting one.
/// It is also where the bug was.
fn names_this_game(environ: &[u8], game: &Game) -> bool {
    let wanted = [
        format!("WINEPREFIX={}", game.pfx().display()),
        format!("STEAM_COMPAT_DATA_PATH={}", game.at.display()),
    ];
    environ.split(|byte| *byte == 0).any(|value| {
        let value = String::from_utf8_lossy(value);
        let value = value.trim_end_matches('/');
        wanted
            .iter()
            .any(|want| want.trim_end_matches('/') == value)
    })
}

/// Whether anything is using this game, which is the one question an
/// uninstall has to ask.
pub fn in_use(game: &Game) -> bool {
    !users(game).is_empty()
}

/// How long everything in a prefix gets to end politely before it is ended
/// outright.
///
/// The shell's own Close waits four seconds, and this is not that: by the
/// time this is asked the application is already over, and what is left is
/// whatever did not notice. Wine's own services go in well under a second —
/// measured — and the wait only ever runs its full length when something is
/// genuinely stuck.
const GRACE: Duration = Duration::from_secs(3);

/// How often to look again while waiting the grace out.
const LOOK: Duration = Duration::from_millis(100);

/// How many of those looks a `SIGKILL` gets before what is left is reported
/// as having survived it. Half a second: the kernel is not slow about this,
/// and anything still here after it is not going.
const AFTER_THE_KILL: u32 = 5;

/// What ending a prefix came to.
///
/// Three counts of one family, narrowing: everything that was asked to end,
/// what had to be killed, and what was still there after that. Each is a
/// subset of the one before it, because a process that appears part way
/// through is asked too rather than being killed without warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ended {
    /// How many processes were asked to end, over the whole wait — a Wine on
    /// its way out can start something, and whatever turned up was asked too.
    pub found: usize,
    /// How many were still there when the grace was up, and were killed.
    pub killed: usize,
    /// How many are still there after all of it.
    ///
    /// Nothing of this user's own survives `SIGKILL`, so this is zero on any
    /// ordinary machine — which is exactly why it is counted. A process
    /// wedged in the kernel is the one case where "everything was ended" is
    /// a lie, and somebody reading the log about a machine that is still
    /// slow has to be able to tell that case from the others.
    pub left: usize,
}

/// Leave nothing of this game running.
///
/// **This is what keeps a Windows game from outliving the screen it was on.**
/// Wine is not one process: a `wineserver` holds the prefix open and goes on
/// holding it for as long as any client is alive, so an application that
/// leaves one behind — a launcher that did not notice, a service that hung on
/// the way out — leaves a whole Wine running with nothing on screen to say so.
/// Valve's supervisor takes the family down with it when it is *signalled*,
/// and cannot when it is killed: `SIGKILL` is not deliverable to a handler,
/// and measured here, a supervisor killed outright left `bwrap`, the adverb,
/// `wineserver`, Wine's `explorer` and the application itself all running.
///
/// Asked politely first, exactly as Close is. A `wineserver` answers `SIGTERM`
/// by shutting its session down properly, which is how a game's registry and
/// its saves survive being closed.
pub fn end(game: &Game) -> Ended {
    let mut asked: BTreeSet<i32> = users(game).into_iter().collect();
    if asked.is_empty() {
        return Ended::default();
    }
    for pid in &asked {
        signal(*pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline {
        let still = users(game);
        if still.is_empty() {
            return Ended {
                found: asked.len(),
                killed: 0,
                left: 0,
            };
        }
        // **Everything gets asked before anything is killed**, including
        // whatever turned up while this was waiting. A Wine on its way out
        // starts things — a client that outlives its `wineserver` is given
        // another one — and a process that appeared after the first round
        // and was then killed outright would be the one process in the
        // family that never got to write anything out.
        for pid in still {
            if asked.insert(pid) {
                signal(pid, libc::SIGTERM);
            }
        }
        std::thread::sleep(LOOK);
    }
    // Asked again rather than reusing the first list: a process that has gone
    // has given its number back, and three seconds is long enough for the
    // machine to have handed it to somebody else's program. The environment
    // is what identifies a process here, so asking again is what makes that
    // impossible rather than merely unlikely.
    let killed = users(game);
    for pid in &killed {
        signal(*pid, libc::SIGKILL);
    }
    // And one last look, because "killed" above is only what was *signalled*.
    // A kill is not instant — the kernel has to get round to it — so this
    // waits the same short while before deciding that something survived it.
    let mut left = Vec::new();
    for _ in 0..AFTER_THE_KILL {
        std::thread::sleep(LOOK);
        left = users(game);
        if left.is_empty() {
            break;
        }
    }
    Ended {
        found: asked.len(),
        killed: killed.len(),
        left: left.len(),
    }
}

/// One signal, to one process of this user's own.
fn signal(pid: i32, signal: i32) {
    // SAFETY: `kill` with a positive pid signals that one process and nothing
    // else; a pid that has gone answers ESRCH, which is the ordinary way this
    // ends and is not an error here.
    unsafe {
        libc::kill(pid, signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_becomes_a_directory_somebody_can_read() {
        assert_eq!(slug("Stardew Valley"), "stardew-valley");
        assert_eq!(slug("Portal 2 (Steam)"), "portal-2-steam");
        assert_eq!(slug("FINAL FANTASY VII™"), "final-fantasy-vii");
        assert_eq!(slug("  "), "windows-game");
        assert_eq!(slug("!!!"), "windows-game");
        assert!(slug(&"x".repeat(300)).len() <= 65);
    }

    /// The same game installed twice is two games, and the first one's
    /// directory is not touched.
    #[test]
    fn a_second_copy_gets_a_number_rather_than_the_first_ones_folder() {
        let root = tempfile::tempdir().unwrap();
        let one = make(root.path(), "Stardew Valley", None).unwrap();
        let two = make(root.path(), "Stardew Valley", None).unwrap();
        assert_eq!(one.id, "stardew-valley");
        assert_eq!(two.id, "stardew-valley-2");
        assert!(one.at.join("lxb.json").is_file());
        assert_eq!(read(&one.at).unwrap().kept.name, "Stardew Valley");
    }

    /// A prefix Proton finished is ready; one an installer died inside is
    /// listed, and says so, because **Run .exe inside of...** is how it is
    /// rescued.
    #[test]
    fn an_interrupted_install_is_listed_and_not_mistaken_for_a_finished_one() {
        let root = tempfile::tempdir().unwrap();
        let mut game = make(root.path(), "Half Installed", None).unwrap();
        std::fs::create_dir_all(game.pfx()).unwrap();
        assert_eq!(game.state(), State::Unfinished);
        assert!(
            !game.prefix_built(),
            "the installer died before Proton finished"
        );

        // Proton got as far as building the prefix, and the installer still
        // left nothing anybody can play.
        std::fs::write(game.at.join("version"), "11.0-100\n").unwrap();
        assert_eq!(game.state(), State::Unfinished);
        assert!(game.prefix_built());

        game.kept.play = Some("pfx/drive_c/Game/game.exe".into());
        game.save().unwrap();
        assert_eq!(read(&game.at).unwrap().state(), State::Ready);

        // And a game imported as a folder is playable before Proton has
        // ever run in it: the prefix is built on the first launch.
        let imported = {
            let mut game = make(root.path(), "Imported", None).unwrap();
            game.kept.play = Some("pfx/drive_c/Games/Imported/game.exe".into());
            game.save().unwrap();
            read(&game.at).unwrap()
        };
        assert!(!imported.prefix_built());
        assert_eq!(imported.state(), State::Ready);
        assert_eq!(
            read(&game.at).unwrap().target().unwrap(),
            game.at.join("pfx/drive_c/Game/game.exe")
        );
    }

    /// A directory nobody here made is not a game, however Windows-shaped
    /// it looks — the `lxb.json` is what says it is ours.
    #[test]
    fn a_directory_that_is_not_ours_is_not_a_game() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("somebody-elses/pfx")).unwrap();
        make(root.path(), "Ours", None).unwrap();
        let found = all(&[root.path().to_path_buf()]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, "ours");
    }

    /// Games under every root, the folder new ones go in first, and a root
    /// that is not there today is passed over rather than being an error.
    #[test]
    fn every_root_is_read_and_a_missing_one_is_not_a_failure() {
        let home = tempfile::tempdir().unwrap();
        let now = home.path().join("disk/Games");
        let before = home.path().join(".lxb-proton");
        make(&now, "New Place", None).unwrap();
        make(&before, "Old Place", None).unwrap();

        let found = all(&[now.clone(), home.path().join("unplugged"), before.clone()]);
        let ids: Vec<&str> = found.iter().map(|game| game.id.as_str()).collect();
        assert_eq!(ids, ["new-place", "old-place"]);
    }

    /// Two roots holding a directory of the same name hold one game, and it
    /// is the one in the folder new games go in.
    #[test]
    fn the_first_root_wins_a_name_that_is_in_both() {
        let home = tempfile::tempdir().unwrap();
        let now = home.path().join("disk/Games");
        let before = home.path().join(".lxb-proton");
        let mut one = make(&now, "Doom", None).unwrap();
        one.kept.name = "Doom, the new one".into();
        one.save().unwrap();
        make(&before, "Doom", None).unwrap();

        let found = all(&[now, before]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kept.name, "Doom, the new one");
    }

    /// One game, somewhere harmless, for the questions below.
    fn game_at(at: &str) -> Game {
        Game {
            id: "cemu".into(),
            at: PathBuf::from(at),
            kept: Kept {
                name: "Cemu".into(),
                play: None,
                arguments: Vec::new(),
                proton: None,
                came_from: None,
                disc: None,
            },
        }
    }

    /// Nothing is using a game nothing has ever run in, and ending it is a
    /// question that answers itself.
    #[test]
    fn a_prefix_nothing_is_running_in_is_free() {
        let root = tempfile::tempdir().unwrap();
        let game = game_at(&root.path().to_string_lossy());
        assert!(users(&game).is_empty());
        assert!(!in_use(&game));
        assert_eq!(end(&game), Ended::default());
    }

    /// **A `WINEPREFIX` written with a trailing slash is the same prefix.**
    ///
    /// The bug this file carried from the first day: Wine writes it with a
    /// slash — measured, on a live `wineserver` — and this helper writes it
    /// without, so the strict comparison that used to be here matched nothing
    /// at all, and the question an uninstall asks quietly answered *nothing is
    /// running in there* about every game on the machine.
    #[test]
    fn a_wineprefix_written_with_a_trailing_slash_is_the_same_prefix() {
        let game = game_at("/home/someone/.lxb-proton/cemu");
        let environ = |line: &str| format!("HOME=/home/someone\0{line}\0LANG=C\0").into_bytes();

        assert!(
            names_this_game(
                &environ("WINEPREFIX=/home/someone/.lxb-proton/cemu/pfx/"),
                &game
            ),
            "the slash Wine writes"
        );
        assert!(
            names_this_game(
                &environ("WINEPREFIX=/home/someone/.lxb-proton/cemu/pfx"),
                &game
            ),
            "and the one this helper writes"
        );
    }

    /// The container around the Wine is this game's too, and it is named by
    /// the other variable: Valve's supervisor and `bwrap` carry
    /// `STEAM_COMPAT_DATA_PATH` and never hear of a prefix.
    #[test]
    fn the_container_round_the_wine_is_found_by_the_game_it_was_given() {
        let game = game_at("/home/someone/.lxb-proton/cemu");
        let environ = |line: &str| format!("PATH=/usr/bin\0{line}\0").into_bytes();
        assert!(names_this_game(
            &environ("STEAM_COMPAT_DATA_PATH=/home/someone/.lxb-proton/cemu"),
            &game
        ));
    }

    /// **And somebody else's game is somebody else's.** The one mistake this
    /// must never make: everything it finds is about to be signalled.
    #[test]
    fn another_game_in_the_same_folder_is_not_this_one() {
        let game = game_at("/home/someone/.lxb-proton/cemu");
        for stranger in [
            "WINEPREFIX=/home/someone/.lxb-proton/cemu-2/pfx",
            "WINEPREFIX=/home/someone/.lxb-proton/notepad/pfx/",
            "STEAM_COMPAT_DATA_PATH=/home/someone/.lxb-proton/notepad",
            "STEAM_COMPAT_DATA_PATH=/home/someone/.lxb-proton",
            "WINEPREFIX=/home/someone/.lxb-proton/cemu",
            "SOMETHING=/home/someone/.lxb-proton/cemu/pfx",
        ] {
            let environ = format!("HOME=/home/someone\0{stranger}\0").into_bytes();
            assert!(!names_this_game(&environ, &game), "{stranger}");
        }
        assert!(!names_this_game(b"", &game), "a process that says nothing");
    }
}
