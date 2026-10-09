//! What to run, and in what environment, to start a Windows game.
//!
//! This helper does not start anything — the shell does, so the game gets
//! the loading screen, the display it was started on and the guide's Close,
//! exactly as an Epic game or a ROM does. What this works out is the argv
//! and the environment, and hands them over.
//!
//! ## The shape
//!
//! ```text
//! <runtime>/pressure-vessel/bin/steam-runtime-supervisor \
//!     --subreaper --terminate-when-signaled -- \
//!   <runtime>/_v2-entry-point --verb=<verb> -- <proton>/proton <verb> <exe> [args]
//! ```
//!
//! The inner two layers are read off a real launch rather than invented:
//! that is the argv Steam's own log shows for a Proton game on this machine,
//! and the two `commandline` keys in the two `toolmanifest.vdf` files say
//! exactly how they are joined — the runtime's `/_v2-entry-point
//! --verb=%verb% --`, then the Proton's `/proton %verb%`, each prefixed by
//! its own install directory, with `%verb%` substituted.
//!
//! ## A Windows desktop for a setup
//!
//! A setup — the installer that makes a prefix, and anything run inside one
//! afterwards — is given a Wine desktop the size of the display to open on:
//!
//! ```text
//!   … <proton>/proton <verb> explorer /desktop=<name>,<w>x<h> <exe> [args]
//! ```
//!
//! **Without it an installer is drawn into the corner of the screen.** Every
//! application window on this session is the size of its display, which is
//! right for a game and wrong for a dialog: a Windows installer is a fixed
//! layout that cannot be made larger, so Wine grows the window and the
//! installer goes on drawing its 294 by 153 pixels at the top-left of it.
//! Measured on a real one — Notepad++'s, whose window says `min` and `max`
//! are both that size, stretched to the whole of a 1280 by 800 display with
//! its buttons in the corner.
//!
//! Inside a desktop there is a window that *can* take the display's size —
//! Wine's own — and the installer is centred in it by the thing that has
//! always centred it, which is the installer. Measured the same way: a
//! desktop asked for at 640 by 480 put the same window at +173+163, dead
//! centre, and one asked for at the size of the screen puts it in the middle
//! of the screen.
//!
//! It is the size at the moment the window opens that decides where it lands:
//! Wine follows the display being resized under it, but a window already up
//! stays where it was put. So the size is the caller's to pass, and the shell
//! passes the display the press was made on.
//!
//! **The game itself is not given one**, only its setups. A game is the thing
//! the screen is for, and one that changes resolution has to be able to; a
//! desktop in the way of that is a game drawing a small picture into the
//! corner of a large one, which is the bug above with another name.
//!
//! ## Why the supervisor is on the outside
//!
//! Nothing in that chain is a subreaper, and two separate things go wrong
//! without one. Both were measured against Valve's own supervisor with a
//! stand-in launcher that starts a long-running child and exits at once:
//!
//! - **A launcher that exits early.** A great many Windows games — and
//!   nearly every installer — run a small executable that starts the real
//!   one and returns. Proton's chain then returns too, and the shell would
//!   file the game as finished while it is on screen. `--subreaper` makes
//!   the supervisor wait for the whole family instead.
//! - **Closing the game.** The shell ends an application by signalling what
//!   it started. Measured: with `--subreaper` alone the family outlives that
//!   signal — the game kept running after the supervisor was gone — so Close
//!   would leave a game nobody can see and nothing can reach.
//!   `--terminate-when-signaled` takes the family down with it.
//!
//! `--terminate-with-main` is the one that looks right and is not: it fires
//! when the *main* command exits, which is precisely the moment an early
//! launcher hands over. Measured, it killed the stand-in game instantly,
//! which is the first failure above rather than a fix for it.
//!
//! ## The environment
//!
//! `proton` refuses to start without exactly two variables — measured
//! against the script itself, which exits on the first and raises a bare
//! `KeyError` on the second:
//!
//! - `STEAM_COMPAT_DATA_PATH`, the game's own directory, whose `pfx/` child
//!   becomes `WINEPREFIX`.
//! - `STEAM_COMPAT_CLIENT_INSTALL_PATH`, which must *exist as a variable*
//!   and need not point at anything. Proton copies Steam's own DLLs out of
//!   it where they are there and carries on where they are not, so a machine
//!   with no Steam gets a prefix with no Steam in it, which is correct.
//!
//! ## What is deliberately not set
//!
//! **`SteamAppId`, `SteamGameId` and `STEAM_COMPAT_APP_ID`.** It is tempting
//! to invent a number, because Wine names a game's window `steam_app_<id>`
//! from one and a distinct window class per game would be convenient. It is
//! also wrong twice over. `proton` reads those variables to decide which
//! *per-title workarounds* to apply, so a made-up number silently opts a
//! game into another game's hacks. And the shell reads `steam_app_<n>` back
//! the other way to decide a window belongs to a Steam game, so a fabricated
//! id would file somebody's Windows game as Steam's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::prefix::Game;
use crate::report::Tool;

/// Which of Proton's verbs a launch uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Wait for any `wineserver` still holding this prefix, then run.
    ///
    /// The verb for anything the shell starts on its own — a game, and an
    /// installer — because the alternative races the previous launch's Wine
    /// as it shuts down.
    WaitForExitAndRun,
    /// `wine <exe>`, in a prefix that is already set up, with no prefix
    /// creation, upgrade or DLL re-sync.
    ///
    /// What **Run something inside a game** wants: the one verb that runs the
    /// named executable directly rather than under Proton's own `steam.exe`
    /// stub, and the one that neither builds the prefix nor upgrades it.
    ///
    /// Measured against a prefix Proton had already built, running
    /// `winver.exe` out of its own `system32`: the three files it touched
    /// were `system.reg`, `user.reg` and `userdef.reg`, which Wine rewrites
    /// on any start at all, and Proton's own `version`, `config_info` and
    /// `tracked_files` were left as they were. So "changes nothing" means no
    /// prefix work rather than no writes — a prefix cannot be run in without
    /// Wine touching its registry.
    RunInPrefix,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::WaitForExitAndRun => "waitforexitandrun",
            Verb::RunInPrefix => "runinprefix",
        }
    }
}

/// The Windows desktop a setup is given to open on, in pixels.
///
/// The display the press was made on, which the shell knows and this helper
/// does not. Wine would otherwise pick its own default and an installer would
/// be centred on a screen nobody has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desktop {
    pub width: u32,
    pub height: u32,
}

impl Desktop {
    /// What `explorer` is told: which desktop, and how big.
    fn switch(self, called: &str) -> String {
        format!(
            "/desktop={},{}x{}",
            desktop_name(called),
            self.width,
            self.height
        )
    }
}

/// The name a desktop is created under, out of whatever the game is called.
///
/// Wine makes this the window's title — `Notepad++ - Wine Desktop` — and the
/// name of the desktop object inside the prefix, so it is cut down to what
/// both can take. **The comma above all**, which is what separates the name
/// from the size and would otherwise let a game called `Hello, World` ask for
/// a desktop called `Hello` of size `World`.
fn desktop_name(called: &str) -> String {
    let kept: String = called
        .chars()
        .map(|letter| match letter {
            letter if letter.is_alphanumeric() => letter,
            ' ' | '+' | '-' | '_' | '.' => letter,
            _ => ' ',
        })
        .collect();
    let trimmed: String = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = trimmed.chars().take(40).collect();
    match short.trim().is_empty() {
        true => "Windows".to_string(),
        false => short.trim().to_string(),
    }
}

/// Everything needed to start one executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The whole command, program first.
    pub argv: Vec<String>,
    /// What to add to the environment. Nothing is removed.
    pub environment: BTreeMap<String, String>,
    /// Where to run it.
    ///
    /// The executable's own directory. `proton` never changes directory
    /// itself, so whatever the caller is in is what the game gets — and a
    /// great many Windows games read their data relative to it and simply
    /// do not start anywhere else.
    pub directory: PathBuf,
}

/// How Windows would start this file: the program, and whatever has to go
/// in front of it.
///
/// Nearly always the file itself — a `.exe` is a program and Wine runs it,
/// and a `.bat` or a `.cmd` is handed to `cmd` by `CreateProcess` without
/// anybody asking, which was measured here rather than assumed.
///
/// **A `.msi` is the exception, and it is not a program at all.** It is a
/// database of what to install, and the thing that reads it is
/// `msiexec` — so a `.msi` handed straight to Wine answers *"Application
/// could not be started, or no application associated with the specified
/// file"*, which is what somebody picking a mod's installer used to get.
/// `msiexec` takes the path in the form it is given, including a Unix one;
/// measured against Wine's own `msiexec` under both of this file's verbs.
///
/// No `/quiet`: an installer that was asked for is an installer somebody
/// wants to answer. The shell puts a loading screen up and the window
/// comes up on it like any other.
fn how_windows_runs(exe: &Path) -> Vec<String> {
    let ending = exe
        .extension()
        .and_then(|end| end.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let path = exe.to_string_lossy().into_owned();
    match ending.as_str() {
        "msi" => vec!["msiexec".to_string(), "/i".to_string(), path],
        _ => vec![path],
    }
}

/// Why a launch could not be worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cannot {
    /// The Proton names a runtime this machine does not have.
    NoRuntime,
    /// Nothing has been named to run.
    NoTarget,
}

/// How to run `exe` under `tool`, in `game`'s prefix.
///
/// `store` is where this helper keeps its own things; a directory inside it
/// stands in for Steam's installation, so `pressure-vessel` has a real path
/// to bind rather than a missing one to warn about on every launch.
///
/// `desktop` is the Windows desktop to open it on, for a setup. `None` runs
/// the executable against the screen itself, which is what a game wants — see
/// the note at the top of this file.
pub fn of(
    game: &Game,
    tool: &Tool,
    exe: &Path,
    arguments: &[String],
    verb: Verb,
    store: &Path,
    desktop: Option<Desktop>,
) -> Result<Launch, Cannot> {
    let runtime = tool
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.entry_point.clone())
        .ok_or(Cannot::NoRuntime)?;

    let mut argv = Vec::new();
    // The outermost layer, where the runtime has one. A runtime without a
    // supervisor still starts its games: family tracking is worth having and
    // not worth refusing a launch over.
    if let Some(supervisor) = supervisor(&runtime) {
        argv.push(supervisor.to_string_lossy().into_owned());
        argv.push("--subreaper".to_string());
        argv.push("--terminate-when-signaled".to_string());
        argv.push("--".to_string());
    }
    argv.extend([
        runtime.clone(),
        format!("--verb={}", verb.word()),
        "--".to_string(),
        tool.bin.clone(),
        verb.word().to_string(),
    ]);
    // Inside a Windows desktop, where one was asked for. `explorer` makes the
    // desktop and then starts what follows it on that desktop, so everything
    // below goes on being the command line it already was.
    if let Some(desktop) = desktop {
        argv.push("explorer".to_string());
        argv.push(desktop.switch(&game.kept.name));
    }
    argv.extend(how_windows_runs(exe));
    argv.extend(arguments.iter().cloned());

    let mut environment = BTreeMap::new();
    let mut set = |key: &str, value: String| {
        environment.insert(key.to_string(), value);
    };
    set(
        "STEAM_COMPAT_DATA_PATH",
        game.at.to_string_lossy().into_owned(),
    );
    // A directory of our own rather than nothing: Proton only reads
    // `legacycompat/` out of it and tolerates its absence, but
    // `pressure-vessel` warns about every path it is told to bind that is
    // not there, and a line of warning on every launch is a line somebody
    // will chase.
    let client = store.join("client");
    let _ = std::fs::create_dir_all(&client);
    set(
        "STEAM_COMPAT_CLIENT_INSTALL_PATH",
        client.to_string_lossy().into_owned(),
    );

    // The host paths the container must be able to see. Both tools, because
    // the chain runs one out of the other; and the game's own directory,
    // which is only needed where it is somewhere `pressure-vessel` does not
    // already share — but naming it always is a string, and working out
    // whether this particular path falls under one of its defaults is a
    // guess about another program's internals.
    let tools: Vec<String> = [proton_root(&tool.bin), runtime_root(&runtime), Some(client)]
        .into_iter()
        .flatten()
        .map(|at| at.to_string_lossy().into_owned())
        .collect();
    set("STEAM_COMPAT_TOOL_PATHS", tools.join(":"));
    // The game's own directory, and — where the executable is not in it —
    // the folder that executable came out of.
    //
    // **The second one is what makes a program from the disk at large
    // runnable in a prefix at all.** An installer, a mod's setup, a
    // patcher: those live in somebody's Downloads, not inside the game, and
    // a container that cannot see the file cannot run it. Said here rather
    // than by each caller so the two ways in cannot drift: a setup given a
    // new prefix and a program run inside an old one are the same problem.
    let mut mounts = vec![game.at.to_string_lossy().into_owned()];
    if !exe.starts_with(&game.at) {
        // A parent of nothing is what a bare filename has, and an empty
        // element in this list is a path the runtime would try to bind.
        if let Some(folder) = exe.parent().filter(|at| !at.as_os_str().is_empty()) {
            mounts.push(folder.to_string_lossy().into_owned());
        }
    }
    set("STEAM_COMPAT_MOUNTS", mounts.join(":"));
    // The container's own scratch copy, in our store rather than inside the
    // runtime: a runtime fetched by this helper should stay exactly as it
    // was checked, and a runtime belonging to Steam is not ours to write
    // into at all.
    let scratch = store.join("containers");
    let _ = std::fs::create_dir_all(&scratch);
    set(
        "PRESSURE_VESSEL_VARIABLE_DIR",
        scratch.to_string_lossy().into_owned(),
    );

    Ok(Launch {
        argv,
        environment,
        directory: exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| game.at.clone()),
    })
}

/// How to start the game itself.
///
/// No Windows desktop: the game is what the display is for. See the note at
/// the top of this file.
pub fn play(game: &Game, tool: &Tool, store: &Path) -> Result<Launch, Cannot> {
    let exe = game.target().ok_or(Cannot::NoTarget)?;
    of(
        game,
        tool,
        &exe,
        &game.kept.arguments,
        Verb::WaitForExitAndRun,
        store,
        None,
    )
}

/// The directory a Proton's `proton` script is in.
fn proton_root(bin: &str) -> Option<PathBuf> {
    Path::new(bin).parent().map(Path::to_path_buf)
}

/// The directory a runtime's entry point is in.
fn runtime_root(entry_point: &str) -> Option<PathBuf> {
    Path::new(entry_point).parent().map(Path::to_path_buf)
}

/// Valve's own process supervisor, which every runtime carries beside its
/// entry point, or `None` where this one does not.
///
/// Taken from the runtime rather than looked for on `PATH`: it is a part of
/// the runtime, versioned with it, and a different one from somewhere else is
/// not a thing to guess at. `pressure-vessel/` is the one built for this
/// machine — the tree carries a `pressure-vessel-arm64/` beside it, which is
/// for a machine this is not.
fn supervisor(entry_point: &str) -> Option<PathBuf> {
    let at = runtime_root(entry_point)?.join("pressure-vessel/bin/steam-runtime-supervisor");
    at.is_file().then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Origin, Runtime};

    fn tool(with_runtime: bool) -> Tool {
        Tool {
            name: "Proton 11.0".into(),
            bin: "/steam/common/Proton 11.0/proton".into(),
            origin: Origin::Valve,
            version: None,
            runtime: Some(Runtime {
                app_id: 4183110,
                suite: Some("steamrt4".into()),
                entry_point: with_runtime
                    .then(|| "/steam/common/SteamLinuxRuntime_4/_v2-entry-point".to_string()),
                version: None,
                origin: Some(Origin::Valve),
            }),
        }
    }

    fn game(at: &Path) -> Game {
        Game {
            id: "stardew-valley".into(),
            at: at.to_path_buf(),
            kept: crate::prefix::Kept {
                name: "Stardew Valley".into(),
                play: Some("pfx/drive_c/Game/Stardew.exe".into()),
                arguments: vec!["--windowed".into()],
                proton: None,
                came_from: None,
            },
        }
    }

    /// The argv Steam's own log shows, layer for layer.
    #[test]
    fn the_command_is_the_runtime_wrapping_the_proton_wrapping_the_game() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let launch = play(&game, &tool(true), &home.path().join("store")).unwrap();
        assert_eq!(
            launch.argv,
            [
                "/steam/common/SteamLinuxRuntime_4/_v2-entry-point",
                "--verb=waitforexitandrun",
                "--",
                "/steam/common/Proton 11.0/proton",
                "waitforexitandrun",
                &*game
                    .at
                    .join("pfx/drive_c/Game/Stardew.exe")
                    .to_string_lossy(),
                "--windowed",
            ]
        );
        assert_eq!(
            launch.directory,
            game.at.join("pfx/drive_c/Game"),
            "a game that reads its data relative to itself has to start there"
        );
    }

    /// The two variables Proton will not start without, and the prefix it
    /// builds under the first.
    #[test]
    fn the_two_variables_proton_demands_are_both_set() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let launch = play(&game, &tool(true), &home.path().join("store")).unwrap();
        assert_eq!(
            launch.environment.get("STEAM_COMPAT_DATA_PATH").unwrap(),
            &game.at.to_string_lossy().into_owned(),
            "the game's own directory, whose pfx/ becomes WINEPREFIX"
        );
        let client = launch
            .environment
            .get("STEAM_COMPAT_CLIENT_INSTALL_PATH")
            .expect("proton raises a bare KeyError without this one");
        assert!(
            Path::new(client).is_dir(),
            "and it is a directory that exists, so pressure-vessel does not \
             warn about binding it on every launch"
        );
    }

    /// The one thing that must never be invented. A fabricated app id opts
    /// the game into another title's workarounds inside Proton, and files
    /// its window as a Steam game inside the shell.
    #[test]
    fn no_steam_app_id_is_invented() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let launch = play(&game, &tool(true), &home.path().join("store")).unwrap();
        for never in ["SteamAppId", "SteamGameId", "STEAM_COMPAT_APP_ID"] {
            assert!(
                !launch.environment.contains_key(never),
                "{never} was set, which is the one thing this must not do"
            );
        }
    }

    /// **Run .exe inside of...** uses the verb that touches nothing: no
    /// prefix creation, no upgrade, and the executable run directly rather
    /// than under Proton's `steam.exe` stub.
    #[test]
    fn running_something_in_an_existing_prefix_leaves_the_prefix_alone() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let exe = game.at.join("pfx/drive_c/Game/config.exe");
        let launch = of(
            &game,
            &tool(true),
            &exe,
            &[],
            Verb::RunInPrefix,
            &home.path().join("store"),
            None,
        )
        .unwrap();
        assert_eq!(launch.argv[1], "--verb=runinprefix");
        assert_eq!(launch.argv[4], "runinprefix");
        assert_eq!(
            launch.argv.last().unwrap(),
            &exe.to_string_lossy().into_owned()
        );
    }

    /// A Proton whose runtime is not on the disk cannot start anything, and
    /// says so rather than producing a command that would fail inside a
    /// container where nothing can see it.
    #[test]
    fn a_proton_without_its_runtime_is_refused_here_and_not_in_the_container() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        assert_eq!(
            play(&game, &tool(false), &home.path().join("store")),
            Err(Cannot::NoRuntime)
        );
    }

    /// A runtime that carries Valve's supervisor is wrapped in it, outside
    /// everything else — including the container, which is the only place it
    /// can see the whole family from.
    ///
    /// The flags are the measured pair, not the obvious one. See the module
    /// note: `--terminate-with-main` fires exactly when an early launcher
    /// hands over to the real game.
    #[test]
    fn the_supervisor_wraps_the_whole_chain_from_the_outside() {
        let home = tempfile::tempdir().unwrap();
        let runtime = home.path().join("SteamLinuxRuntime_4");
        std::fs::create_dir_all(runtime.join("pressure-vessel/bin")).unwrap();
        let supervisor = runtime.join("pressure-vessel/bin/steam-runtime-supervisor");
        std::fs::write(&supervisor, b"").unwrap();
        let entry = runtime.join("_v2-entry-point");

        let mut tool = tool(true);
        if let Some(runtime) = tool.runtime.as_mut() {
            runtime.entry_point = Some(entry.to_string_lossy().into_owned());
        }
        let game = game(&home.path().join("games/stardew-valley"));
        let launch = play(&game, &tool, &home.path().join("store")).unwrap();

        assert_eq!(
            &launch.argv[..4],
            &[
                supervisor.to_string_lossy().into_owned(),
                "--subreaper".to_string(),
                "--terminate-when-signaled".to_string(),
                "--".to_string(),
            ],
            "the supervisor and its two measured flags come first"
        );
        assert_eq!(
            launch.argv[4],
            entry.to_string_lossy().into_owned(),
            "and the runtime entry point is what it supervises"
        );
        assert!(
            !launch.argv.contains(&"--terminate-with-main".to_string()),
            "--terminate-with-main kills the game the moment a launcher exits"
        );
    }

    /// A runtime with no supervisor beside it still starts its games. Family
    /// tracking is worth having and not worth refusing a launch over.
    #[test]
    fn a_runtime_without_a_supervisor_still_starts_the_game() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let launch = play(&game, &tool(true), &home.path().join("store")).unwrap();
        assert_eq!(
            launch.argv[0], "/steam/common/SteamLinuxRuntime_4/_v2-entry-point",
            "nothing in front of the entry point when there is no supervisor to put there"
        );
    }

    /// **A program the container cannot see is a program that cannot run.**
    ///
    /// Everything somebody runs inside an existing prefix — a mod's setup, a
    /// patcher, an installer that wants the game already there — is a file
    /// in their own Downloads, outside the game entirely. The container is
    /// told about the folder it came out of, beside the game's own, or Wine
    /// is handed a path to nothing.
    #[test]
    fn a_program_from_outside_the_game_is_mounted_so_the_container_can_see_it() {
        let home = tempfile::tempdir().unwrap();
        let at = home.path().join("games/stardew-valley");
        let game = game(&at);
        let store = home.path().join("store");

        let downloaded = home.path().join("Downloads/graphics-pack-setup.exe");
        let launch = of(
            &game,
            &tool(true),
            &downloaded,
            &[],
            Verb::RunInPrefix,
            &store,
            None,
        )
        .expect("a runtime is here");
        let mounts = launch
            .environment
            .get("STEAM_COMPAT_MOUNTS")
            .expect("the container is told what to bind");
        let named: Vec<&str> = mounts.split(':').collect();
        assert!(
            named.contains(&at.to_string_lossy().as_ref()),
            "the game's own folder: {named:?}"
        );
        assert!(
            named.contains(&home.path().join("Downloads").to_string_lossy().as_ref()),
            "and the folder the program came out of: {named:?}"
        );

        // A program already inside the game needs nothing extra, and saying
        // its folder again would be the same disk named twice.
        let within = at.join("pfx/drive_c/Games/Stardew/config.exe");
        let launch = of(
            &game,
            &tool(true),
            &within,
            &[],
            Verb::RunInPrefix,
            &store,
            None,
        )
        .expect("a runtime is here");
        assert_eq!(
            launch
                .environment
                .get("STEAM_COMPAT_MOUNTS")
                .map(|at| at.as_str()),
            Some(at.to_string_lossy().as_ref()),
            "the game's own folder, and nothing else"
        );
    }

    /// A game whose prefix Proton has never built needs it built, even for
    /// the verb whose whole point is to build nothing.
    ///
    /// `runinprefix` runs the executable directly — no `setup_prefix`, no
    /// DLL overrides — so in a game that has never been played it would
    /// have bare Wine make the prefix instead of Proton. A game imported as
    /// a folder is exactly that until the first time it is played, and an
    /// interrupted install may be too, and both are games somebody reaches
    /// for **Run something inside** on.
    #[test]
    fn the_verb_depends_on_whether_there_is_a_prefix_to_run_in_yet() {
        let home = tempfile::tempdir().unwrap();
        let at = home.path().join("games/stardew-valley");
        let game = game(&at);
        assert!(!game.prefix_built(), "nothing has run in here");

        // Proton writes `version` last, and it is what it reads back
        // itself — see `prefix::Game::prefix_built`.
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(
            at.join("version"),
            "11.0-100
",
        )
        .unwrap();
        assert!(game.prefix_built(), "and now something has");
    }

    /// **A `.msi` is not a program.** It is a database of what to install,
    /// and the thing that reads it is `msiexec` — Wine handed one directly
    /// answers "Application could not be started, or no application
    /// associated with the specified file", which is what somebody picking
    /// a mod's installer used to get. Everything else goes as itself: a
    /// `.bat` is handed to `cmd` by `CreateProcess` without anybody asking,
    /// measured rather than assumed.
    #[test]
    fn a_windows_installer_package_is_run_by_the_program_that_reads_one() {
        assert_eq!(
            how_windows_runs(Path::new("/d/patch.msi")),
            ["msiexec", "/i", "/d/patch.msi"]
        );
        assert_eq!(
            how_windows_runs(Path::new("/d/PATCH.MSI")),
            ["msiexec", "/i", "/d/PATCH.MSI"],
            "a disk written by Windows shouts"
        );
        for itself in ["/d/setup.exe", "/d/mod.bat", "/d/mod.cmd", "/d/whatever"] {
            assert_eq!(how_windows_runs(Path::new(itself)), [itself], "{itself}");
        }

        // And it reaches the argv, which is the whole point of it.
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let msi = home.path().join("Downloads/patch.msi");
        let launch = of(
            &game,
            &tool(true),
            &msi,
            &[],
            Verb::RunInPrefix,
            &home.path().join("store"),
            None,
        )
        .expect("a runtime is here");
        let tail = &launch.argv[launch.argv.len() - 3..];
        assert_eq!(
            tail,
            ["msiexec", "/i", msi.to_string_lossy().as_ref()],
            "{:?}",
            launch.argv
        );
    }

    /// **A setup opens on a Windows desktop of the display's size**, which is
    /// what puts an installer in the middle of the screen instead of in the
    /// corner of it. `explorer` comes between the verb and the executable,
    /// and everything after it is the command line it was always going to be.
    #[test]
    fn a_setup_is_run_on_a_windows_desktop_the_size_of_the_display() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let setup = home.path().join("Downloads/setup.exe");
        let launch = of(
            &game,
            &tool(true),
            &setup,
            &["/S".to_string()],
            Verb::WaitForExitAndRun,
            &home.path().join("store"),
            Some(Desktop {
                width: 1280,
                height: 800,
            }),
        )
        .expect("a runtime is here");
        let tail = &launch.argv[launch.argv.len() - 4..];
        assert_eq!(
            tail,
            [
                "explorer",
                "/desktop=Stardew Valley,1280x800",
                &*setup.to_string_lossy(),
                "/S",
            ],
            "{:?}",
            launch.argv
        );
    }

    /// And a `.msi`, whose program is `msiexec` rather than the file, is
    /// started on that desktop as itself: the desktop is in front of the
    /// whole command line rather than in the middle of it.
    #[test]
    fn a_windows_installer_package_opens_on_the_desktop_too() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let msi = home.path().join("Downloads/patch.msi");
        let launch = of(
            &game,
            &tool(true),
            &msi,
            &[],
            Verb::RunInPrefix,
            &home.path().join("store"),
            Some(Desktop {
                width: 1920,
                height: 1080,
            }),
        )
        .expect("a runtime is here");
        let tail = &launch.argv[launch.argv.len() - 5..];
        assert_eq!(
            tail,
            [
                "explorer",
                "/desktop=Stardew Valley,1920x1080",
                "msiexec",
                "/i",
                &*msi.to_string_lossy(),
            ],
            "{:?}",
            launch.argv
        );
    }

    /// **The game itself is not given a desktop.** It is the thing the
    /// display is for, and a game that changes resolution inside a desktop
    /// draws a small picture into the corner of a large one — the bug this
    /// whole arrangement exists to fix, with another name.
    #[test]
    fn the_game_itself_is_run_against_the_screen_and_not_on_a_desktop() {
        let home = tempfile::tempdir().unwrap();
        let game = game(&home.path().join("games/stardew-valley"));
        let launch = play(&game, &tool(true), &home.path().join("store")).unwrap();
        assert!(
            !launch.argv.iter().any(|part| part == "explorer"),
            "{:?}",
            launch.argv
        );
    }

    /// A desktop's name is the game's, cut down to what Wine and the comma in
    /// `/desktop=name,WxH` can both take.
    #[test]
    fn a_desktops_name_cannot_be_read_as_part_of_its_size() {
        assert_eq!(desktop_name("Notepad++"), "Notepad++");
        assert_eq!(desktop_name("Stardew Valley"), "Stardew Valley");
        assert_eq!(
            desktop_name("Hello, World"),
            "Hello World",
            "a comma would end the name and start the size"
        );
        assert_eq!(
            desktop_name("  Tom\tClancy's   Splinter  Cell "),
            "Tom Clancy s Splinter Cell",
            "one space between words, and none at either end"
        );
        assert_eq!(desktop_name(""), "Windows");
        assert_eq!(desktop_name("///"), "Windows");
        assert_eq!(
            desktop_name(&"a".repeat(80)),
            "a".repeat(40),
            "a name no object can take is no name"
        );

        // And the name is what reaches the switch.
        assert_eq!(
            Desktop {
                width: 800,
                height: 600,
            }
            .switch("Hello, World"),
            "/desktop=Hello World,800x600"
        );
    }

    /// A game nothing has been named to run in is not a launch.
    #[test]
    fn a_game_with_nothing_named_to_run_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let mut game = game(&home.path().join("games/stardew-valley"));
        game.kept.play = None;
        assert_eq!(
            play(&game, &tool(true), &home.path().join("store")),
            Err(Cannot::NoTarget)
        );
    }
}
