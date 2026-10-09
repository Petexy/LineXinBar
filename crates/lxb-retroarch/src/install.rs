//! Installing RetroArch, as the one command that needs nobody's permission.
//!
//! **Into this user's own flatpak installation, always.** A distribution
//! package would need root, which on a console means a password panel over a
//! television for a program the user has just said they want; a system-wide
//! flatpak would need polkit, which is the same thing wearing a different hat.
//! A `--user` install is the one route that is entirely this person's own
//! business, and it is why the shell can offer the install as a plain Yes.
//!
//! ## Flathub has to be a remote of *theirs*
//!
//! Nearly every machine with flatpak on it has Flathub, and on nearly every one
//! of them it is a system remote. A user installation cannot install from
//! somebody else's remote, so the first thing here adds Flathub to this user's
//! own — which is `--if-not-exists`, costs nothing on the second run, and is
//! the same command every flatpak page on the internet opens with.
//!
//! ## What the shell is told
//!
//! One [`Progress`] line per change, on stdout. flatpak's own output goes to
//! this process's stderr, which is the session log: what it prints is a
//! progress bar drawn with carriage returns, and the only thing in it worth
//! putting on a panel is the percentage — which is what [`percentage`] takes
//! out of it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::find::FLATPAK_ID;
use crate::report::{Installation, Kind, Permission, Progress, Stage, PROTOCOL};

/// Where Flathub is, for a user who has not got it.
const FLATHUB: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// Install it, saying what is happening as it happens.
///
/// Returns whether it ended installed. Every line written here is one JSON
/// object, and the last one is always `Done` or `Failed` — a shell watching
/// this stream has to be able to tell that it is over without watching the
/// process as well.
pub fn run(out: &mut impl Write) -> bool {
    tell(out, Stage::Remote, None, "Getting ready".to_string());
    if let Err(why) = remote() {
        tell(out, Stage::Failed, None, why);
        return false;
    }

    tell(
        out,
        Stage::Installing,
        None,
        "Downloading RetroArch".to_string(),
    );
    match install(out) {
        Ok(()) => {
            tell(
                out,
                Stage::Done,
                Some(1.0),
                "RetroArch is ready".to_string(),
            );
            true
        }
        Err(why) => {
            tell(out, Stage::Failed, None, why);
            false
        }
    }
}

/// RetroArch has gone; take what this integration kept for it.
///
/// The other end of [`run`], and the half of a removal that is nobody else's
/// to do. **The application itself is removed by the shell**, which is the only
/// part of this that can ask for a password: it knows how this RetroArch was
/// installed and hands it back to whatever installed it, so a distribution
/// package and a system-wide flatpak come off exactly as the `--user` flatpak
/// this helper installs does. What is left over is everything *outside* that
/// program, and no package manager and no `--delete-data` reaches any of it.
///
/// What goes, always:
///
/// * The filesystem permission the shell granted for the games folder, which
///   `flatpak override --user --reset` undoes. It survives an uninstall
///   otherwise and a reinstall would silently inherit it. Only the user's own
///   overrides are reset, because a `--user` override is the only one the shell
///   ever wrote — a system-wide installation's are per-installation and need
///   root, and are somebody else's to undo.
/// * This shell's own cache of the cover art it fetched from libretro, which
///   lives under the *shell's* cache rather than RetroArch's and would
///   otherwise be the one thing a reinstall did not have to fetch again.
/// * The scratch directory a core's own files land in while it is being asked
///   what it can be set to.
///
/// And with `data`, which is the shell's **Remove everything**: RetroArch's own
/// configuration directory outside any sandbox, `~/.config/retroarch` — its
/// settings, the cores downloaded into it, the playlists, the saves and any
/// BIOS put there. A flatpak's own directory has already gone with it, because
/// the shell passed `--delete-data`; a distribution package's never had anybody
/// to take it, because a package manager removes what its package shipped and
/// nothing else. Where a machine has had both, both go, which is what the
/// button that was pressed says.
///
/// The user's games are not touched, and neither is the folder they are in.
/// Those are theirs; this removes what a program kept, not what somebody owns.
///
/// Every failure here is reported and none of them stops the rest: the
/// application is already gone by the time this runs, and a helper that
/// returned "it could not be removed" over a leftover cache would be saying the
/// opposite of what happened.
pub fn forget(out: &mut impl Write, data: bool) -> bool {
    tell(out, Stage::Removing, None, "Clearing up".to_string());
    reset("--user");
    let mut gone = leftovers();
    if data {
        gone.extend(own_config());
    }
    for at in gone {
        match std::fs::remove_dir_all(&at) {
            Ok(()) => eprintln!("forget: removed {}", at.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("forget: {} could not be removed: {err}", at.display()),
        }
    }
    tell(
        out,
        Stage::Done,
        Some(1.0),
        "RetroArch has been removed".to_string(),
    );
    true
}

/// RetroArch's own configuration directory on a machine where it was not a
/// flatpak — the one `find::config_dir` answers with for [`Kind::System`],
/// worked out here without an installation because by now there is none.
fn own_config() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    Some(home.join(".config/retroarch"))
}

/// Give back the folder permission the shell granted.
///
/// It is an override on the application id and outlives the application: a
/// machine that installed RetroArch again would find it already allowed to read
/// a folder nobody had pointed it at this time round.
fn reset(scope: &str) {
    let out = Command::new("flatpak")
        .args(["override", scope, "--reset", FLATPAK_ID])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(out) if out.status.success() => eprintln!("uninstall: permissions reset"),
        Ok(out) => eprintln!(
            "uninstall: permissions were not reset: {}",
            said(&out.stderr).unwrap_or_else(|| "flatpak refused".to_string())
        ),
        Err(err) => eprintln!("uninstall: flatpak could not be run: {err}"),
    }
}

/// What this integration keeps outside RetroArch's own directory.
///
/// Everything here belongs to the shell rather than to the emulator, which is
/// exactly why `flatpak uninstall --delete-data` does not reach it: the cover
/// art was fetched by this helper into the shell's cache, and the scratch
/// directory is where a core's own files land while it is being asked what it
/// can be set to. Both would survive into a fresh install and make it look
/// like less of a fresh install than it was.
fn leftovers() -> Vec<std::path::PathBuf> {
    let mut here = Vec::new();
    if let Some(art) = crate::art::cache() {
        here.push(art);
    }
    here.push(std::env::temp_dir().join("lxb-retroarch-asking"));
    here
}

/// Let the emulator read the folder the user has just chosen.
///
/// A flatpak sees this user's home directory and nothing else, so a collection
/// on an external drive — or anywhere else outside it — is one RetroArch starts
/// and then cannot open, reporting it in its own window where nobody is looking.
/// `flatpak override --user` widens that, for this one application, to the one
/// folder the user has just pointed at. It needs no authority of any kind and is
/// undone with `flatpak override --user --reset org.libretro.RetroArch`.
///
/// Read *and* write, deliberately: RetroArch's own default is to keep a save
/// beside the game it belongs to, and a folder it could only read would be a
/// game somebody could play and not save.
///
/// A distribution package is in no sandbox and needs none of this, which is what
/// `installation` decides.
pub fn permit(at: &Path, installation: Option<&Installation>) -> Permission {
    let scope = match installation.map(|installation| installation.kind) {
        Some(Kind::FlatpakUser) => "--user",
        // A system-wide flatpak's overrides are per-installation and writing
        // them needs root, so this one is left to whoever installed it. The
        // user's own home is reachable either way, which is where nearly every
        // collection is.
        Some(Kind::FlatpakSystem) => {
            return say(true, "a system flatpak keeps the permissions it was given")
        }
        Some(Kind::System) => return say(true, "a distribution package is in no sandbox"),
        None => return say(false, "there is no RetroArch to give access to"),
    };

    let out = Command::new("flatpak")
        .args([
            "override",
            scope,
            &format!("--filesystem={}", at.display()),
            FLATPAK_ID,
        ])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(out) if out.status.success() => {
            say(true, &format!("granted access to {}", at.display()))
        }
        Ok(out) => say(
            false,
            &said(&out.stderr).unwrap_or_else(|| "flatpak refused".to_string()),
        ),
        Err(err) => say(false, &format!("flatpak could not be run: {err}")),
    }
}

fn say(permitted: bool, note: &str) -> Permission {
    Permission {
        protocol: PROTOCOL,
        permitted,
        note: note.to_string(),
    }
}

/// Make sure this user can see Flathub.
fn remote() -> Result<(), String> {
    let out = Command::new("flatpak")
        .args([
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            FLATHUB,
        ])
        // See the note on the install below: what flatpak says is read by this
        // and shown by the shell, and both want one known language.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("flatpak could not be run: {err}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(said(&out.stderr).unwrap_or_else(|| "The download could not be started".to_string()))
}

/// The install itself, with the percentage forwarded as it changes.
fn install(out: &mut impl Write) -> Result<(), String> {
    let mut child = Command::new("flatpak")
        .args(["install", "--user", "-y", "flathub", FLATPAK_ID])
        // In one language, because both halves of what flatpak says here are
        // read by something: [`operation`] parses the line that says which of
        // its jobs it is on, and the last line of its complaint is what the
        // panel shows when an install fails. Neither should turn on whether the
        // user's session happens to be in German.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("flatpak could not be run: {err}"))?;

    // Read on this thread, and keep the last few lines of stderr: what flatpak
    // says when it refuses is one sentence, and it is the sentence the panel
    // has to show.
    let mut complaint = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            let mut kept: Vec<String> = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("flatpak: {line}");
                let line = line.trim().to_string();
                if !line.is_empty() {
                    kept.push(line);
                    // Only the end of it is ever shown, and an install that
                    // fails after twenty minutes of downloading must not have
                    // twenty minutes of complaint held in memory.
                    if kept.len() > 8 {
                        kept.remove(0);
                    }
                }
            }
            kept
        })
    });

    if let Some(stdout) = child.stdout.take() {
        // Which job of how many, carried between chunks: flatpak writes the
        // pair on the same line as the bar, but a redraw of the bar alone is a
        // chunk with no pair in it.
        let mut job = (1u32, 1u32);
        let mut last = None;
        for chunk in chunks(stdout) {
            if let Some(said) = operation(&chunk) {
                job = said;
            }
            if let Some(percent) = percentage(&chunk) {
                let (at, of) = job;
                let done = whole(at, of, percent);
                if last != Some(done) {
                    last = Some(done);
                    tell(
                        out,
                        Stage::Installing,
                        Some(done as f32 / 100.0),
                        note(at, of),
                    );
                }
            }
        }
    }

    let status = child
        .wait()
        .map_err(|err| format!("flatpak could not be waited for: {err}"))?;
    let kept = complaint
        .take()
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    Err(kept
        .last()
        .cloned()
        .unwrap_or_else(|| "RetroArch could not be downloaded".to_string()))
}

/// Which of flatpak's jobs a line is about, and how many there are.
///
/// It prints `Installing 2/4… ███░░░  45%`: one job per thing it has to fetch —
/// a runtime, its extensions, the application — each with a bar of its own that
/// starts again at nothing. A shell drawing that percentage alone draws a bar
/// which fills and empties three or four times over with the same sentence
/// under it, and what that reads as is an install that keeps starting again.
///
/// Read in English on purpose: the command is run under `LC_ALL=C`, because the
/// word in front of the numbers is translated and this would otherwise work in
/// some countries and not others.
fn operation(line: &str) -> Option<(u32, u32)> {
    let rest = ["Installing", "Updating"]
        .iter()
        .find_map(|word| line.split_once(word).map(|(_, rest)| rest.trim_start()))?;
    let counted: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '/')
        .collect();
    let (at, of) = counted.split_once('/')?;
    let (at, of) = (at.parse::<u32>().ok()?, of.parse::<u32>().ok()?);
    // A job numbered past the end, or none at all, is a line this has misread.
    (of > 0 && at >= 1 && at <= of).then_some((at, of))
}

/// How far along the whole install is, from which job it is on and how far
/// along that one is.
///
/// The jobs are not the same size — a runtime is hundreds of megabytes and an
/// extension is one — so this is not the true fraction of the bytes. It is
/// monotonic, which is the property that matters: a bar that only ever goes
/// forwards is telling the truth about an install that is only ever getting
/// nearer to done.
fn whole(at: u32, of: u32, percent: u8) -> u8 {
    let of = of.max(1);
    let done = (at.saturating_sub(1) as f32 + percent as f32 / 100.0) / of as f32;
    (done * 100.0).round().clamp(0.0, 100.0) as u8
}

/// What to call what is happening, given which job it is.
///
/// flatpak installs what RetroArch needs before RetroArch, so everything but
/// the last job is the runtime and its parts. Saying "Downloading RetroArch"
/// through all of it is the other half of why the old bar read as a lie.
fn note(at: u32, of: u32) -> String {
    match at < of {
        true => "Getting what RetroArch needs".to_string(),
        false => "Downloading RetroArch".to_string(),
    }
}

/// flatpak's progress bar, split where it actually ends.
///
/// It is drawn by rewriting one line with carriage returns, so `lines()` on it
/// yields nothing at all until the whole install is over. Splitting on both
/// terminators is what turns it back into a series of states, and the last one
/// of each is the current percentage.
fn chunks(stream: impl Read) -> impl Iterator<Item = String> {
    let mut reader = BufReader::new(stream);
    std::iter::from_fn(move || {
        let mut raw = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            match reader.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    if byte[0] == b'\r' || byte[0] == b'\n' {
                        if raw.is_empty() {
                            continue;
                        }
                        break;
                    }
                    raw.push(byte[0]);
                }
                Err(_) => break,
            }
        }
        (!raw.is_empty()).then(|| String::from_utf8_lossy(&raw).into_owned())
    })
}

/// The last percentage written in a line of flatpak's, if it wrote one.
fn percentage(line: &str) -> Option<u8> {
    let mut found = None;
    let bytes: Vec<char> = line.chars().collect();
    for (at, c) in bytes.iter().enumerate() {
        if *c != '%' {
            continue;
        }
        let mut start = at;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == at {
            continue;
        }
        let number: String = bytes[start..at].iter().collect();
        if let Ok(percent) = number.parse::<u32>() {
            found = Some(percent.min(100) as u8);
        }
    }
    found
}

/// What a failed command said, as one line.
fn said(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    Some(line.to_string())
}

fn tell(out: &mut impl Write, stage: Stage, progress: Option<f32>, note: String) {
    let line = Progress {
        protocol: PROTOCOL,
        stage,
        progress,
        note,
    };
    if let Ok(json) = serde_json::to_string(&line) {
        let _ = writeln!(out, "{json}");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What is taken off besides the application, and what is not.
    ///
    /// The point of the row this is behind is starting again from nothing, and
    /// two things would otherwise survive it: the cover art this helper fetched
    /// into the *shell's* cache, which `flatpak uninstall --delete-data` cannot
    /// reach because it is not RetroArch's, and the scratch directory a core's
    /// own files land in while it is being asked what it can be set to. A
    /// reinstall that already had every cover is not a reinstall anybody can
    /// test the first run against.
    #[test]
    fn what_survives_an_uninstall_is_taken_too() {
        let here = leftovers();
        let scratch = std::env::temp_dir().join("lxb-retroarch-asking");
        assert!(here.contains(&scratch), "the scratch directory: {here:?}");
        if let Some(art) = crate::art::cache() {
            assert!(here.contains(&art), "the cover art: {here:?}");
        }
        // And nothing of the user's own. Every path here is under a cache or a
        // temporary directory; a games folder or a home directory in this list
        // would be this helper deleting somebody's collection.
        for at in &here {
            let path = at.to_string_lossy();
            assert!(
                path.contains("cache")
                    || path.starts_with(&*std::env::temp_dir().to_string_lossy()),
                "{path} is not somewhere this may delete"
            );
        }
    }

    /// flatpak's four jobs become one bar that only goes forwards.
    ///
    /// The bug: it prints `Installing 1/4… 100%` and then `Installing 2/4… 0%`,
    /// so a panel showing that percentage filled and emptied four times over
    /// with "Downloading RetroArch" under it the whole way. Somebody watching
    /// it has no way to read that as anything but an install going wrong.
    #[test]
    fn four_jobs_make_one_bar_that_only_goes_forwards() {
        assert_eq!(operation("Installing 2/4…  45%"), Some((2, 4)));
        assert_eq!(operation("Updating 1/2… ███░  10%"), Some((1, 2)));
        // The file counter inside a job is not the job counter: that pair is
        // three files of twelve, and it belongs to whichever job is running.
        assert_eq!(
            operation("Installing 2/4… Downloading files: 3/12 x"),
            Some((2, 4)),
            "the pair after the word, not the next pair on the line"
        );
        assert_eq!(operation("Downloading files: 3/12"), None);
        assert_eq!(operation("Installing…  45%"), None, "no pair at all");
        assert_eq!(operation("Installing 5/4…"), None, "past the end");
        assert_eq!(
            operation("Installing 0/4…"),
            None,
            "and jobs count from one"
        );

        // Nothing anywhere in here goes backwards.
        let mut was = 0;
        for (at, of) in [(1u32, 4u32), (2, 4), (3, 4), (4, 4)] {
            for percent in [0u8, 50, 100] {
                let now = whole(at, of, percent);
                assert!(now >= was, "{at}/{of} at {percent}% went backwards");
                was = now;
            }
        }
        assert_eq!(whole(1, 4, 100), 25, "one job of four done is a quarter");
        assert_eq!(whole(4, 4, 100), 100);
        assert_eq!(whole(1, 1, 40), 40, "and one job is just itself");
    }

    /// And it stops claiming to be downloading RetroArch while it is
    /// downloading the runtime RetroArch needs.
    #[test]
    fn the_runtime_is_not_called_retroarch() {
        assert_eq!(note(1, 4), "Getting what RetroArch needs");
        assert_eq!(note(3, 4), "Getting what RetroArch needs");
        assert_eq!(note(4, 4), "Downloading RetroArch");
        assert_eq!(note(1, 1), "Downloading RetroArch", "nothing else to get");
    }

    /// The one thing parsed out of flatpak's output, taken from the shapes it
    /// actually writes: a bar with the number at the end, and the plain line a
    /// piped run prints.
    #[test]
    fn the_percentage_is_read_off_the_bar() {
        assert_eq!(percentage("Installing… ████░░░░  45%"), Some(45));
        assert_eq!(percentage("100%"), Some(100));
        // Two on one line is the bar being rewritten without a terminator
        // between; the later one is the current state.
        assert_eq!(percentage("12% ... 34%"), Some(34));
        assert_eq!(percentage("Installing"), None);
        assert_eq!(percentage("%"), None);
    }

    /// A carriage-returned bar is a series of states rather than one line at
    /// the end of the install.
    #[test]
    fn a_rewritten_line_comes_back_as_its_states() {
        let raw = "Installing\r 10%\r 40%\r100%\nDone\n";
        let seen: Vec<String> = chunks(raw.as_bytes()).collect();
        assert_eq!(seen, vec!["Installing", " 10%", " 40%", "100%", "Done"]);
    }
}
