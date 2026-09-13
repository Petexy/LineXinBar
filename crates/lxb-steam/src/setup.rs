//! Steam installing itself, the once, with nobody watching it do it.
//!
//! Valve ships two things under the name `steam`. What a distribution packages
//! is a launcher — a shell script and a twenty-megabyte bootstrap tarball — and
//! the client proper is half a gigabyte that the launcher fetches the first
//! time anybody runs it. So a machine with Steam "installed" has no Steam on it
//! at all until someone starts it once, and the first start is a download, an
//! unpack and an install taking anything from forty seconds to the better part
//! of an hour.
//!
//! On an ordinary desktop that first start is a window with a progress bar in
//! it, and the person who double-clicked Steam watches it. This shell has no
//! such person: it keeps Valve's client out of sight on purpose — see
//! [`crate::client`] — and the client's own furniture is exactly what must
//! never reach the screen. What is left is this module, which does the first
//! start on a thread and says how it is going, so the shell can draw the wait
//! in its own panel and in its own words.
//!
//! ## The word that made it impossible
//!
//! Until 2026-09-04 the shell could not install Steam at all, and the reason
//! was one of the four words it says to every client it starts. `-noverifyfiles`
//! means "do not re-check your own installation", which on an ordinary start
//! saves minutes of disk — and on a *first* start means "do not notice that you
//! are not installed". The bootstrap skips its verification, finds no
//! `steamui.so` to load, and dies in under a second. See
//! `client::QUIETLY_FIRST_RUN`, which is the same list without it, and
//! which this module is what made necessary.
//!
//! ## What can be known while it runs
//!
//! Three things, and only the first two are worth anything:
//!
//! * **The updater's own log**, `logs/bootstrap_log.txt`, is written a line at a
//!   time throughout: `Downloading update (443,219 of 496,367 KB)...`, then
//!   `Extracting package...`, `Installing update...`, `Cleaning up...`. That is
//!   where [`Step`] comes from.
//! * **Whether the process is still there.** This module keeps the child rather
//!   than dropping it, which [`crate::client::start`] does not, because a first
//!   start is the one start that can fail in a second and leave nothing behind
//!   to read. A launcher that has exited without finishing is the whole of the
//!   failure detection, and it needs no prose.
//! * The client's own connection log, which does not exist yet and says nothing
//!   about any of this.
//!
//! **No window appears while this runs**, which was measured rather than hoped
//! for: with no `steamui.so` on the disk the updater has no graphical UI to put
//! up and logs `Using update UI: console`, falling back to printing its
//! progress. It is the *second* start — the one after the install, and every
//! ordinary update after that — that logs `Using update UI: xwin` and shows a
//! window, and that window carries `WM_CLASS` `steam` like the rest of the
//! client's and is hidden with them.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::client::{Options, Where};

/// Valve's own log of its first start, relative to the Steam root.
const BOOTSTRAP_LOG: [&str; 2] = ["logs", "bootstrap_log.txt"];

/// How often the log is read while the install runs.
///
/// A second, because that is roughly how often the updater writes to it while
/// downloading and there is nothing to be gained by asking faster. It is a
/// four-kilobyte read of the tail of one file on a thread of its own.
const LOOK: Duration = Duration::from_secs(1);

/// How long the log may say nothing at all before this gives up on it.
///
/// The last resort, and deliberately far past what any real step takes. Nearly
/// every way a first setup can end badly ends with the launcher *exiting* — the
/// updater gives up on packages it cannot fetch, says so and stops — and that
/// is caught in [`UNTIL_A_GONE_LAUNCHER_IS_A_FAILURE`] seconds by asking the
/// kernel rather than by waiting. What is left for this is the one case nothing
/// else can see: a launcher that is alive and doing nothing, for ever.
///
/// So it is long, because the two mistakes are not the same size. Waiting ten
/// minutes on a download that has died costs a panel somebody can walk away
/// from with one press; giving up ten minutes into an unpack that was going to
/// finish kills a working install of half a gigabyte and asks them to fetch it
/// again.
const UNTIL_A_QUIET_SETUP_IS_STUCK: Duration = Duration::from_secs(600);

/// How long the launcher may be gone before its absence is a failure.
///
/// Not nothing, because the launcher does not run the whole install in one
/// process: the bootstrap fetches the client, then hands over to it — and this
/// shell's own [`crate::client::restart`] is not the only thing that can leave
/// a gap where no Steam is running. What makes this safe rather than a guess is
/// that it is only ever *reached* on a run that has not finished, and the
/// finish is tested first.
const UNTIL_A_GONE_LAUNCHER_IS_A_FAILURE: Duration = Duration::from_secs(20);

/// How long the marker is left in place after the install is over, for the
/// client to read it.
///
/// [`finished`] is true the moment the client's furniture is on the disk and
/// something answers the pipe, and that is a fraction of a second *before* the
/// client that was just installed reads the marker and opens its port —
/// `config/` appeared at 20:28:43.5 and the port opened at 20:28:44.3,
/// measured on 2026-09-13, with the setup's one-second look landing anywhere
/// in between. A marker taken back in that gap is a client that comes up with
/// no interface, which the first press then has to stop and start again to
/// get one. So the marker stays until the port answers, or until this has
/// waited longer than any client takes to open one.
const UNTIL_THE_CLIENT_READS_THE_MARKER: Duration = Duration::from_secs(30);

/// Whether Valve's client has yet to install itself where this one keeps
/// itself.
///
/// **Not "is the directory there".** The shell makes that directory itself, one
/// line before it starts the client — see [`crate::client::expose`] — and a
/// first start that ran out of patience leaves it behind with a marker in it.
/// What ends a first run is Valve's own furniture arriving, which is what
/// [`crate::library::looks_like_a_root`] asks about.
pub fn needed(options: &Options) -> bool {
    !crate::library::looks_like_a_root(&options.root)
}

/// How far along the first setup is, as the updater's own log says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// What is happening, in the shell's words rather than Valve's. See
    /// [`said_about`].
    pub said: String,
    /// How much of it has been done, in whole percent, where the line carried
    /// two numbers.
    ///
    /// `None` for every step that is not the download — an unpack says nothing
    /// about its own length, and a bar that sat at 100% through it would be
    /// worse than no bar.
    ///
    /// Whole percent rather than a share, and that is not only about deriving
    /// [`Eq`]. It is the granularity Valve's own updater reports in (`Set
    /// percent complete: 96`), it is the granularity a bar on a television is
    /// read at, and it is what makes "say something when it changes" mean
    /// something: a share taken off two byte counts changes on every line of
    /// the log, five hundred times over one download.
    pub percent: Option<u8>,
}

/// What the shell is told while this runs, and at the end of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetUp {
    /// It is under way. Sent on every change and not on every look, so a shell
    /// that redraws on one of these redraws when something moved.
    Working(Step),
    /// Valve's client is installed on this machine. It is also *running*, and
    /// exposing the interface a sign-in is handed over through — this module
    /// starts it that way on purpose, so the first login costs no second start.
    Done,
    /// It could not be, and this is what to tell whoever asked.
    Failed(String),
}

/// Do the first setup, on this thread, saying how it goes.
///
/// Long. Minutes at best, and this blocks for the whole of it — the caller is
/// [`crate::Steam`]'s worker, which gives it a thread of its own.
///
/// `say` is called for every change and for the ending. It is called at least
/// once: a setup that fails before Valve's launcher has written a word still
/// answers, because something raised a panel to wait for this.
pub fn run(client: &Where, options: &Options, say: impl Fn(SetUp)) {
    match install(client, options, &say) {
        Ok(()) => {
            tracing::info!(
                root = %options.root.display(),
                "Valve's client has finished setting itself up"
            );
            say(SetUp::Done);
        }
        Err(why) => {
            tracing::warn!(%why, "Valve's client could not set itself up");
            say(SetUp::Failed(why));
        }
    }
}

/// The whole of it, with the ending as a `Result` so the one place above can
/// turn it into the one event.
fn install(client: &Where, options: &Options, say: &impl Fn(SetUp)) -> Result<(), String> {
    // The same turn a wake takes, held for the whole of it, and for a stronger
    // reason than a wake has: this unpacks half a gigabyte into the directory a
    // wake would be starting a client out of. Two of them at once is two of
    // Valve's launchers writing over each other's install, and the two *can*
    // meet — a session set to start Steam with the shell wakes a client at
    // startup, on a machine where there is no client, at the same moment
    // somebody presses the sign-in row.
    //
    // It is a lock on the machine rather than on this process, so it also
    // covers the other session and this crate's own `probe-*` examples. See
    // [`crate::turns`] and [`crate::client::wake`], which is the other holder.
    let _turn = crate::turns::take(
        crate::turns::What::TheClient,
        crate::turns::Behalf {
            backend: Some(options.root.clone()),
            request: None,
        },
    );

    // Asked again now the turn is held, because the wait for it may have been a
    // wake that installed the whole client. Answering `Done` to a setup that
    // somebody else did is the honest answer and is what the caller needs: what
    // was asked for is a machine with Steam on it.
    if !needed(options) {
        tracing::info!("Valve's client was installed while this setup waited its turn");
        return Ok(());
    }

    // Somebody else's launcher is already doing this. Not a race and not an
    // error: a session set to start Steam with the shell wakes a client as it
    // comes up, that wake is what installs one on a machine with none — see
    // [`crate::client::wake`] — and this call is then somebody pressing the
    // sign-in row while that runs. It waited its turn behind that wake and the
    // wake has since let go, so what is left is a launcher unpacking half a
    // gigabyte with nothing watching it.
    //
    // **A second launcher must not be started over it.** Valve's own is
    // single-instance: it finds the pipe, hands its arguments to the one that
    // is running and exits at once — which this would read as a launcher that
    // fell over, and report as a failed install of a client that was installing
    // perfectly well. So the one that is there is watched instead, out of the
    // same log, and the only thing given up is the child to ask about: the
    // stall below is what says it has stopped.
    let already = crate::client::running(&options.home);

    // Before the launcher is started, and that ordering is the whole of why it
    // works: the client reads the marker as it comes up and never looks again.
    // It costs nothing on the download and saves a restart at the end of it —
    // the client this leaves running is one a sign-in can be handed to.
    //
    // It also makes the directory, which is what a first start needs of this
    // shell, and it is the reason [`needed`] cannot be "is the directory
    // there".
    let ours = crate::client::expose(options)?;

    // Kept rather than dropped. See the module docs: a first start is the one
    // that can be over in a second, and a launcher that has exited without
    // finishing is the only failure signal that needs no prose of Valve's.
    let mut launcher = match already {
        true => {
            tracing::info!(
                root = %options.root.display(),
                "Valve's client is already setting itself up; watching that rather than starting another"
            );
            None
        }
        false => {
            let words = crate::client::how_to_start(options);
            tracing::info!(
                root = %options.root.display(),
                "starting Valve's client for the first time on this machine"
            );
            Some(
                client
                    .command()
                    .args(words)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|error| format!("Steam would not start: {error}"))?,
            )
        }
    };

    let outcome = watch(options, say, launcher.as_mut());

    // Whatever happened, the marker is spent — it is read as the client starts
    // and never again — and only what this call created is taken back. The same
    // rule, and the same reason, as the one at the end of a wake. Not the same
    // moment, though: a wake takes it back after it has *used* the port, and
    // an install that has just finished is a client that has not opened its
    // port yet. See [`UNTIL_THE_CLIENT_READS_THE_MARKER`].
    if ours {
        if outcome.is_ok() {
            let deadline = Instant::now() + UNTIL_THE_CLIENT_READS_THE_MARKER;
            while !crate::webui::listening() && Instant::now() < deadline {
                std::thread::sleep(LOOK);
            }
        }
        crate::webui::withdraw(&options.root);
    }
    if outcome.is_err() {
        // A launcher still running behind a setup that failed is a download
        // nobody is watching and a client nobody asked for. Only one this call
        // started: a launcher somebody else's wake started is theirs to end.
        // The ordinary failure is one that has already exited; this is for the
        // stall.
        if let Some(launcher) = launcher.as_mut() {
            let _ = launcher.kill();
        }
    }
    outcome
}

/// Watch it happen, and say when it stops.
///
/// `launcher` is the process this module started, where it started one. `None`
/// says the install was already running when this call arrived, and there is
/// then nothing to ask about it: the stall is the only ending this can see, and
/// it is enough — a launcher that has gone stops writing to the log.
fn watch(
    options: &Options,
    say: &impl Fn(SetUp),
    mut launcher: Option<&mut std::process::Child>,
) -> Result<(), String> {
    let mut said: Option<Step> = None;
    let mut written = 0;
    let mut last_moved = Instant::now();
    let mut gone_since: Option<Instant> = None;

    loop {
        // Asked first, so that a launcher which has already handed over to the
        // client it installed is never mistaken for one that fell over.
        if finished(options) {
            return Ok(());
        }

        // **How long the log is, rather than which step it is on.** The stall
        // below is the only thing that can end a run whose launcher is alive,
        // so what it measures had better be everything the updater does — and
        // a step is not: `Extracting package...` is one line, and the unpack it
        // names writes nothing else for as long as it takes. On a slow disk
        // that is minutes of a `said` that has not changed, which a stall
        // watching the *step* would eventually call a failure and kill a
        // perfectly good install for. Every line the updater writes moves this,
        // including the many this module has no name for.
        let now = std::fs::metadata(log(&options.root)).map_or(0, |file| file.len());
        if now != written {
            written = now;
            last_moved = Instant::now();
        }

        if let Some(step) = read(&options.root) {
            if said.as_ref() != Some(&step) {
                said = Some(step.clone());
                say(SetUp::Working(step));
            }
        }

        // Has the launcher gone? `try_wait` rather than a walk of `/proc`: this
        // is the process this module started, so the kernel will say.
        match launcher.as_mut().map(|launcher| launcher.try_wait()) {
            Some(Ok(Some(_))) => {
                let since = *gone_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= UNTIL_A_GONE_LAUNCHER_IS_A_FAILURE {
                    return Err(why_it_stopped(&options.root));
                }
            }
            // Still there, or the kernel would not say, or there is no child of
            // this call's to ask about — none of which is a failure and none of
            // which is a finish. The stall below covers all three.
            Some(Ok(None)) | Some(Err(_)) | None => gone_since = None,
        }

        if last_moved.elapsed() >= UNTIL_A_QUIET_SETUP_IS_STUCK {
            return Err(
                "Steam stopped part-way through setting itself up. Check this machine's \
                 connection and try again."
                    .to_string(),
            );
        }

        std::thread::sleep(LOOK);
    }
}

/// Whether the install is over and there is a client here now.
///
/// Both halves, because neither is enough on its own. Valve's own furniture
/// arriving says the *files* are installed, and it is written by the unpack a
/// few seconds before the client that was unpacked is up; the pipe answering
/// says *something* is running, and the launcher answers its own pipe for the
/// whole of the download. Together they are what the shell actually wanted from
/// this: an installed client, running, ready to be handed a credential.
fn finished(options: &Options) -> bool {
    crate::library::looks_like_a_root(&options.root) && crate::client::running(&options.home)
}

/// What to say about a launcher that exited without finishing.
///
/// Valve's own last word where it left one, and this shell's where it did not.
/// The updater writes a `Fatal error:` line for everything it gives up on —
/// no room on the disk, packages it could not fetch, an update it had to
/// revert — and that line is a far better answer than any guess made from out
/// here.
fn why_it_stopped(root: &Path) -> String {
    match fatal_line(root) {
        Some(said) => format!("Steam could not set itself up: {said}"),
        None => "Steam stopped while setting itself up, and did not say why.".to_string(),
    }
}

/// The last thing the updater called fatal, if it called anything that.
fn fatal_line(root: &Path) -> Option<String> {
    let text = tail(&log(root))?;
    text.lines()
        .rev()
        .find_map(|line| {
            // Two spellings, both Valve's — `Fatal error: …` from the updater
            // itself and `[----] !!! Fatal Error: …` from its console UI.
            // Matched case-insensitively because that is the whole of what
            // separates them, and by byte offset into the *original* line,
            // which is safe because lowercasing ASCII does not move anything:
            // the phrase is ASCII, and a line that is not is one this never
            // finds the phrase in.
            const PHRASE: &str = "fatal error";
            let at = line.to_lowercase().find(PHRASE)? + PHRASE.len();
            Some(line.get(at..)?.to_string())
        })
        .map(|rest| {
            rest.trim_start_matches(|c: char| c == ':' || c == '!' || c.is_whitespace())
                .trim()
                .to_string()
        })
        .filter(|said| !said.is_empty())
}

/// How far along it is, from the updater's own log.
///
/// `None` where there is nothing to read — the first second or two of a run,
/// and every run on a machine whose Steam directory this shell has only just
/// made.
pub fn read(root: &Path) -> Option<Step> {
    let text = tail(&log(root))?;
    text.lines().rev().find_map(|line| said_about(line.trim()))
}

fn log(root: &Path) -> std::path::PathBuf {
    let mut path = root.to_path_buf();
    for part in BOOTSTRAP_LOG {
        path.push(part);
    }
    path
}

/// How much of the end of that log to read.
///
/// It is one line per few hundred kilobytes of a five-hundred-megabyte
/// download, so by the end of one install it is a couple of hundred kilobytes
/// — and it is appended across every run of Steam this machine ever does,
/// never truncated. Reading all of it once a second is the same mistake the
/// connection log used to be. Sixteen kilobytes is dozens of lines, which is
/// far more than the one this wants.
const TAIL: u64 = 16 * 1024;

/// The last [`TAIL`] bytes of a file, as text.
///
/// Whatever of it is valid UTF-8, and the first line is dropped: a read that
/// starts in the middle of the file starts in the middle of a line, and a
/// half-line is not something to parse a number out of.
fn tail(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let from = length.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::with_capacity(TAIL as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match from == 0 {
        true => Some(text),
        false => text.split_once('\n').map(|(_, rest)| rest.to_string()),
    }
}

/// One line of Valve's log, as a step of the shell's own — or nothing, for the
/// many lines that are not about progress at all.
///
/// **The words are this shell's and the numbers are Valve's**, which is the
/// whole shape of this function. What the updater writes is its own English
/// and its own punctuation — `Downloading update (443,219 of 496,367 KB)...` —
/// and the shell is not going to put that on a panel: half of it is a unit
/// nobody asked about and the other half is a sentence in a language that may
/// not be the one the session is in. What is worth having out of it is *which
/// of four things is happening*, and, for the one step long enough to want a
/// bar, how far along it is.
fn said_about(line: &str) -> Option<Step> {
    // The moment in the line, which every line carries, is not part of the
    // matching: it is a timestamp in brackets and the message follows it.
    let message = line.rsplit_once(']').map_or(line, |(_, rest)| rest).trim();
    let lower = message.to_lowercase();

    if lower.starts_with("downloading update") {
        return Some(Step {
            said: "Downloading Steam".to_string(),
            percent: percent_in(message),
        });
    }
    if lower.starts_with("extracting package") {
        return Some(Step {
            said: "Unpacking Steam".to_string(),
            percent: None,
        });
    }
    if lower.starts_with("installing update") || lower.starts_with("applying update") {
        return Some(Step {
            said: "Installing Steam".to_string(),
            percent: None,
        });
    }
    if lower.starts_with("cleaning up") || lower.starts_with("update complete") {
        return Some(Step {
            said: "Finishing up".to_string(),
            percent: None,
        });
    }
    if lower.starts_with("verifying installation") || lower.starts_with("checking for") {
        return Some(Step {
            said: "Checking what Steam needs".to_string(),
            percent: None,
        });
    }
    None
}

/// The two numbers out of `(443,219 of 496,367 KB)`, as whole percent.
///
/// Thousands separators and all: they are what Valve writes, and a parse that
/// did not expect them would answer `None` for the whole of the one step that
/// has a number worth drawing. The unit is not read — both sides carry the
/// same one, and a ratio does not care what it is.
fn percent_in(message: &str) -> Option<u8> {
    let inside = message.split_once('(')?.1.split_once(')')?.0;
    let (done, total) = inside.split_once(" of ")?;
    let done = number_in(done)?;
    let total = number_in(total)?;
    // Nought of nought is the first line of a download that has not been sized
    // yet, and is not "none of it has arrived".
    let total = (total > 0).then_some(total)?;
    // Rounded down rather than to nearest, so nothing reads 100% while bytes
    // are still arriving.
    Some((done.min(total) * 100 / total).min(100) as u8)
}

/// One of Valve's numbers, which may have commas in it and a unit after it.
fn number_in(text: &str) -> Option<u64> {
    let digits: String = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    match digits.is_empty() {
        true => None,
        false => digits.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines this was written against are real ones, copied out of a
    /// `bootstrap_log.txt` written by Valve's updater on this machine on
    /// 2026-09-04 while it installed itself into an empty home directory.
    #[test]
    fn it_reads_valves_own_lines() {
        let download =
            said_about("[2026-09-04 15:35:05] Downloading update (443,219 of 496,367 KB)...")
                .expect("a download line is a step");
        assert_eq!(download.said, "Downloading Steam");
        assert_eq!(download.percent, Some(89));

        for (line, said) in [
            (
                "[2026-09-04 15:35:11] Extracting package...",
                "Unpacking Steam",
            ),
            (
                "[2026-09-04 15:35:18] Installing update...",
                "Installing Steam",
            ),
            ("[2026-09-04 15:35:25] Cleaning up...", "Finishing up"),
            (
                "[2026-09-04 15:35:25] Update complete, launching...",
                "Finishing up",
            ),
            (
                "[2026-09-04 15:34:12] Verifying installation...",
                "Checking what Steam needs",
            ),
        ] {
            let step = said_about(line).unwrap_or_else(|| panic!("{line} is a step"));
            assert_eq!(step.said, said, "{line}");
            assert_eq!(step.percent, None, "{line} has no share to draw");
        }
    }

    /// The log is mostly lines this is not about, and a step must not be
    /// invented out of one of them.
    #[test]
    fn it_says_nothing_about_the_lines_that_are_not_steps() {
        for line in [
            "[2026-09-04 15:34:12] Startup - updater built Jun 24 2026 23:24:37",
            "[2026-09-04 15:34:12] Verification skipped",
            "[2026-09-04 15:35:10] Download Complete.",
            "[2026-09-04 15:34:12] 1. https://client-update.steamstatic.com, /, Realm 'steamglobal'",
            "",
        ] {
            assert_eq!(said_about(line), None, "{line}");
        }
    }

    /// A download whose size Steam has not settled yet draws no bar, rather
    /// than a full one or a division by nothing.
    #[test]
    fn a_download_with_no_size_yet_has_no_share() {
        let step = said_about("[2026-09-04 15:34:46] Downloading update (0 of 0 KB)...")
            .expect("it is still a download");
        assert_eq!(step.said, "Downloading Steam");
        assert_eq!(step.percent, None);
    }

    /// Valve's own last word is what a failure is reported with, in both the
    /// spellings the updater uses for it.
    #[test]
    fn it_takes_the_reason_out_of_valves_log() {
        let root = std::env::temp_dir().join(format!("lxb-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("logs")).unwrap();

        // Nothing readable at all: a sentence of the shell's own, and never an
        // empty one.
        assert!(why_it_stopped(&root).contains("did not say why"));

        std::fs::write(
            log(&root),
            "[2026-09-04 15:34:12] Verifying installation...\n\
             [2026-09-04 15:34:12] Fatal error: Failed to load steamui.so\n",
        )
        .unwrap();
        assert_eq!(
            fatal_line(&root).as_deref(),
            Some("Failed to load steamui.so")
        );

        // The console UI's spelling of the same line, and the *last* one wins:
        // a run that recovered from one and then died of another is reported
        // by the one that ended it.
        std::fs::write(
            log(&root),
            "[2026-09-04 15:34:12] Fatal error: Failed to load steamui.so\n\
             [2026-09-04 15:36:00] [----] !!! Fatal Error: Not enough disk space\n",
        )
        .unwrap();
        assert_eq!(fatal_line(&root).as_deref(), Some("Not enough disk space"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The tail is read rather than the file, and a read that lands in the
    /// middle of a line does not parse half of one.
    #[test]
    fn only_the_end_of_the_log_is_read() {
        let root = std::env::temp_dir().join(format!("lxb-setup-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("logs")).unwrap();

        // Far more than [`TAIL`] of lines that are not steps, and then the one
        // that is. A reader that gave up at the head of its window would find
        // nothing here.
        let mut text = String::new();
        for index in 0..40_000 {
            text.push_str(&format!(
                "[2026-09-04 15:34:12] PingWebSocketCM() attempt {index}\n"
            ));
        }
        text.push_str("[2026-09-04 15:35:05] Downloading update (100 of 400 KB)...\n");
        std::fs::write(log(&root), &text).unwrap();

        let step = read(&root).expect("the last line is a step");
        assert_eq!(step.said, "Downloading Steam");
        assert_eq!(step.percent, Some(25));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// And a log with nothing in it at all is a setup that has not said
    /// anything yet, not a failure.
    #[test]
    fn a_log_that_is_not_there_yet_says_nothing() {
        assert_eq!(read(Path::new("/nonexistent-steam-root")), None);
    }
}
