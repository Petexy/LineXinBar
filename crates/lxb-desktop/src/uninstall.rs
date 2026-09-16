//! Removing an application: what it would take, whether this user may, and
//! doing it.
//!
//! The shell does not know how to uninstall anything. It knows how to ask the
//! thing that installed it — see [`crate::appinfo`] for how that is worked out
//! — and everything here is a translation of one [`Origin`] into one argv.
//!
//! ## Three answers, in order
//!
//! 1. **What would be run.** [`removal`] turns an origin into a [`Removal`]: a
//!    program and its arguments, and whether it has to be root. Nothing here
//!    ever builds a shell command line. Every removal is an argv handed
//!    straight to `execve`, so a package name is one argument however it is
//!    spelt, and `appinfo` has already refused any name that is not one.
//!
//! 2. **Whether this user may.** [`authority`] asks `sudo` itself rather than
//!    guessing from group membership: a machine can put someone in `wheel` and
//!    not enable the `wheel` rule, or grant sudo through an entirely different
//!    group. `sudo -n -v` answers all three cases in one call without ever
//!    prompting — see [`Authority`].
//!
//! 3. **Doing it.** [`Run`] does the work on a thread, because a package
//!    removal is seconds of disk, and reports one [`Outcome`].
//!
//! ## The password
//!
//! A password typed into a shell is worth being careful with, and the care is
//! all in [`crate::secret::Secret`]: it never reaches a command line, an
//! environment variable or the log, it is handed to `sudo` on a pipe, and the
//! buffer holding it is overwritten when it is dropped.
//!
//! It is also used exactly once, for `sudo -v` and nothing else. That is what
//! makes a wrong password unambiguous — `sudo -v` fails for essentially one
//! reason — and it means the removal itself runs under `sudo -n`, against the
//! timestamp validating has just set, with no password anywhere near it. The
//! cost is that the user's `sudo` is warmed for its usual few minutes
//! afterwards, exactly as it would be had they typed `sudo` in a terminal.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::appinfo::{Manager, Origin, Scope};
use crate::secret::Secret;

/// What removing an application would actually run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// The program and its arguments. Never a shell string.
    pub argv: Vec<String>,
    /// Whether it has to run as root.
    pub root: bool,
    /// The package being removed, which is not always what the application is
    /// called — `nm-connection-editor` installs "Advanced Network
    /// Configuration" — so the panel can show both.
    pub package: String,
}

/// How to remove what `origin` describes, or `None` when the shell has no idea.
///
/// The `--noninteractive` and `--noconfirm` flags are not optional politeness:
/// there is no terminal on the other end of any of these, and a tool that stops
/// to ask would hang until the user gave up.
pub fn removal(origin: &Origin) -> Option<Removal> {
    let (argv, root): (Vec<&str>, bool) = match origin {
        // A user installation belongs to the person logged in, so it comes out
        // with no authority at all. A system one goes through polkit rather
        // than sudo — that is flatpak's own design, and on most distributions
        // an active local user in `wheel` is allowed it outright. Where the
        // policy does want authentication there has to be an agent running to
        // collect it, and a session that is only this shell may not have one;
        // that comes back as a plain failure with polkit's own words in it,
        // which is the honest thing to show.
        Origin::Flatpak { id, scope } => (
            vec![
                "flatpak",
                "uninstall",
                match scope {
                    Scope::User => "--user",
                    Scope::System => "--system",
                },
                "--assumeyes",
                "--noninteractive",
                "--",
                id,
            ],
            false,
        ),
        Origin::Snap { name } => (vec!["snap", "remove", "--", name], true),
        Origin::System { manager, package } => match manager {
            // `-Rs` and not `-R`: it takes the package and the dependencies
            // that nothing else still needs, which is what "remove this
            // application" means to anyone who did not install it by hand. It
            // cannot take something another package depends on — that is the
            // definition of the flag, not a hope about it.
            Manager::Pacman => (vec!["pacman", "--noconfirm", "-Rs", "--", package], true),
            Manager::Dpkg => (vec!["apt-get", "--yes", "remove", "--", package], true),
            // Whichever front end this machine has. Plain `rpm -e` is last:
            // it removes the package and nothing it dragged in, and it will
            // refuse rather than break a dependency, which makes it a poor
            // first choice and a safe final one.
            Manager::Rpm => {
                let front = ["dnf", "zypper", "rpm"]
                    .into_iter()
                    .find(|tool| installed(tool))?;
                match front {
                    "dnf" => (vec!["dnf", "--assumeyes", "remove", "--", package], true),
                    "zypper" => (
                        vec!["zypper", "--non-interactive", "remove", "--", package],
                        true,
                    ),
                    _ => (vec!["rpm", "--erase", "--", package], true),
                }
            }
        },
        Origin::Unknown => return None,
    };
    // The tool has to be here. A flatpak entry on a machine with no `flatpak`
    // is not a thing that happens, but a `Removal` naming a program that does
    // not exist would fail after the user had answered a question about
    // destroying something, which is the worst moment to find out.
    if !installed(argv[0]) {
        return None;
    }
    Some(Removal {
        argv: argv.into_iter().map(str::to_string).collect(),
        root,
        package: origin.package().unwrap_or_default().to_string(),
    })
}

/// What standing this user has for a removal that needs root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// Nothing has to be asked for: the removal does not need root.
    NotNeeded,
    /// Root, and `sudo` will not ask for anything — either the machine is set
    /// up that way, or the user has used `sudo` recently enough.
    Granted,
    /// Root, and `sudo` wants the user's password.
    NeedsPassword,
    /// Root, and this user cannot have it.
    Forbidden,
}

/// Ask `sudo` where this user stands, without prompting for anything.
///
/// `sudo -n -v` is the whole probe. It is the one call that distinguishes all
/// three answers: it succeeds when no password is wanted, and when it fails it
/// says which of the two reasons in a sentence that has been stable for
/// decades. Group membership is deliberately not consulted — being in `wheel`
/// says nothing on a machine that has not enabled the `wheel` rule, and sudo
/// can be granted through a group nobody would think to look at.
///
/// Anything unrecognised is [`Authority::NeedsPassword`], which is the useful
/// way round: the user is offered the chance to authenticate and `sudo` gets
/// the final say. Refusing them on a message this did not recognise would be
/// this function overruling the thing it is asking.
pub fn authority(removal: &Removal) -> Authority {
    if !removal.root {
        return Authority::NotNeeded;
    }
    if !installed("sudo") {
        // A machine with `doas` and no `sudo` lands here. `doas` reads its
        // password from the terminal and offers no way to hand it one, so the
        // shell genuinely cannot get permission — which is the same thing, from
        // where the user is standing, as not having it.
        tracing::info!("no sudo on this system; a removal needing root cannot be authorised");
        return Authority::Forbidden;
    }
    let Ok(output) = Command::new("sudo")
        .args(["-n", "-v"])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
    else {
        return Authority::NeedsPassword;
    };
    if output.status.success() {
        return Authority::Granted;
    }
    let said = String::from_utf8_lossy(&output.stderr).to_lowercase();
    if said.contains("password is required") || said.contains("password required") {
        return Authority::NeedsPassword;
    }
    if said.contains("may not run sudo")
        || said.contains("not allowed to")
        || said.contains("not in the sudoers")
    {
        return Authority::Forbidden;
    }
    tracing::debug!(said = %said.trim(), "sudo said something unfamiliar; asking for a password");
    Authority::NeedsPassword
}

/// Whether a program is on `PATH`.
fn installed(program: &str) -> bool {
    // Asked by running it rather than by walking `PATH`, so that a shell
    // function, an alias directory or an unusual `PATH` in the session is
    // answered the same way the removal itself will be.
    Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// What the shell found out before it can offer to remove anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub removal: Option<Removal>,
    pub authority: Authority,
}

impl Plan {
    /// The plan for something that cannot be removed: nothing to run, and
    /// therefore nothing to authorise. What a worker that never answered comes
    /// back as, and what the shell shows as "we cannot tell what installed
    /// this" — the same answer, and the same safe one, as a package manager
    /// that said so itself.
    fn nothing_to_do() -> Self {
        Self {
            removal: None,
            authority: Authority::NotNeeded,
        }
    }
}

/// The plan, asked for off the render thread.
///
/// Started the moment the question "do you want to uninstall this" goes up
/// rather than when it is answered, so that by the time the user has read the
/// question and pressed a button the answer is already in and they go straight
/// to the panel that fits. It costs two or three short-lived processes per
/// press of Uninstall, which is a fair price for never showing a spinner
/// between a button and its consequence.
pub struct Survey {
    plan: Arc<Mutex<Option<Plan>>>,
    asked: Instant,
}

/// How long the survey is given before the shell stops waiting for it. The
/// same reasoning as [`crate::appinfo`]'s: being slow is normal, waiting for
/// ever is not.
const SURVEY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(6);

impl Survey {
    pub fn start(path: &std::path::Path) -> Self {
        let slot: Arc<Mutex<Option<Plan>>> = Arc::new(Mutex::new(None));
        let into = Arc::clone(&slot);
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let origin = crate::appinfo::identify(&path);
            let found = plan(&origin);
            tracing::debug!(
                ?path,
                ?origin,
                ?found,
                "worked out what removing this takes"
            );
            if let Ok(mut into) = into.lock() {
                *into = Some(found);
            }
        });
        Self {
            plan: slot,
            asked: Instant::now(),
        }
    }

    /// The plan, or `None` while it is still being worked out.
    ///
    /// Once the patience has run out this is a plan with nothing in it, which
    /// the shell shows as "we cannot tell what installed this" — the same
    /// answer, and the same safe one, as a package manager that said so.
    pub fn plan(&self) -> Option<Plan> {
        match self.plan.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => Some(Plan::nothing_to_do()),
        }
        .or_else(|| (self.asked.elapsed() >= SURVEY_PATIENCE).then(Plan::nothing_to_do))
    }
}

/// Work out the plan for `origin`. Costs a process or two; call it off a
/// worker.
pub fn plan(origin: &Origin) -> Plan {
    let removal = removal(origin);
    let authority = match &removal {
        Some(removal) => authority(removal),
        // Nothing to authorise, and nothing that would use the answer.
        None => Authority::NotNeeded,
    };
    Plan { removal, authority }
}

/// How a removal ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Removed,
    /// `sudo` did not accept the password. The one outcome that is worth
    /// telling apart from every other failure, because it is the only one the
    /// user can fix by trying again.
    WrongPassword,
    /// Anything else, with what the tool said about it.
    Failed(String),
}

/// A removal that has been started.
pub struct Run {
    outcome: Arc<Mutex<Option<Outcome>>>,
}

impl Run {
    /// Start `removal`, authenticating with `password` if one was asked for.
    ///
    /// The password is moved in and dropped on the worker the moment `sudo -v`
    /// has been fed it, so it is gone well before the removal itself finishes.
    pub fn start(removal: Removal, password: Option<Secret>) -> Self {
        let outcome = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&outcome);
        std::thread::spawn(move || {
            let result = carry_out(&removal, password);
            match &result {
                Outcome::Removed => {
                    tracing::info!(package = %removal.package, "removed")
                }
                Outcome::WrongPassword => tracing::info!("the password was not accepted"),
                Outcome::Failed(why) => {
                    tracing::warn!(package = %removal.package, why, "the removal failed")
                }
            }
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(result);
            }
        });
        Self { outcome }
    }

    /// How it ended, or `None` while it is still going.
    pub fn outcome(&self) -> Option<Outcome> {
        match self.outcome.lock() {
            Ok(slot) => slot.clone(),
            // A worker that panicked will never answer, and a panel waiting on
            // it for ever is worse than being told it went wrong.
            Err(_) => Some(Outcome::Failed(
                crate::i18n::text("label-the-removal-did-not-finish").to_string(),
            )),
        }
    }
}

/// Authenticate if a password was given, then run the removal.
fn carry_out(removal: &Removal, password: Option<Secret>) -> Outcome {
    if let Some(password) = password {
        if let Some(refused) = validate(password) {
            return refused;
        }
    }
    let mut command = if removal.root {
        // `-n`, so that a timestamp which expired between validating and here
        // comes back as a failure rather than as a process sitting on a prompt
        // nobody can see.
        let mut command = Command::new("sudo");
        command.args(["-n", "--"]);
        command.args(&removal.argv);
        command
    } else {
        let mut command = Command::new(&removal.argv[0]);
        command.args(&removal.argv[1..]);
        command
    };
    let output = command.env("LC_ALL", "C").stdin(Stdio::null()).output();
    match output {
        Ok(output) if output.status.success() => Outcome::Removed,
        Ok(output) => Outcome::Failed(complaint(&output.stderr, &output.stdout)),
        Err(err) => Outcome::Failed(err.to_string()),
    }
}

/// Hand the password to `sudo -v`, and say what it thought. `None` means it was
/// accepted.
fn validate(password: Secret) -> Option<Outcome> {
    let spawned = Command::new("sudo")
        // An empty prompt: there is nobody to read one, and the default would
        // otherwise be printed into the error this shows the user.
        .args(["-S", "-p", "", "-v"])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => return Some(Outcome::Failed(err.to_string())),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = password.hand_to(&mut stdin);
        // Closed here rather than at the end of the function: sudo waits on
        // end-of-input before it gives up on a second attempt.
        drop(stdin);
    }
    // And gone, well before the removal it authorises has finished.
    drop(password);

    match child.wait_with_output() {
        Ok(output) if output.status.success() => None,
        Ok(output) => {
            let said = String::from_utf8_lossy(&output.stderr).to_lowercase();
            // sudo's own words for a password it did not like. Everything else
            // is a real failure and is shown as one, rather than telling the
            // user to try a password that was never the problem.
            if said.contains("try again")
                || said.contains("incorrect password")
                || said.contains("no password was provided")
                || said.contains("authentication failure")
            {
                Some(Outcome::WrongPassword)
            } else {
                Some(Outcome::Failed(complaint(&output.stderr, &[])))
            }
        }
        Err(err) => Some(Outcome::Failed(err.to_string())),
    }
}

/// One line out of what a tool said when it failed, for a panel to show.
///
/// The first line that carries anything, with the program's own prefix taken
/// off — `error: target not found: thing` says the same as `target not found:
/// thing` and the panel has one line to say it in.
fn complaint(stderr: &[u8], stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr).into_owned();
    let text = if text.trim().is_empty() {
        String::from_utf8_lossy(stdout).into_owned()
    } else {
        text
    };
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(crate::i18n::text(
            "label-the-removal-failed-and-said-nothing",
        ));
    let line = line
        .strip_prefix("error:")
        .or_else(|| line.strip_prefix("Error:"))
        .or_else(|| line.strip_prefix("E:"))
        .unwrap_or(line)
        .trim();
    // Long enough to be useful, short enough to be one line on the panel.
    match line.char_indices().nth(120) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invented names throughout: nothing installed on any particular machine
    /// belongs in this file, least of all in the tests for the code that
    /// deletes things.
    const PACKAGE: &str = "example-editor";
    const APP_ID: &str = "com.example.Editor";

    fn argv(origin: &Origin) -> Option<Vec<String>> {
        removal(origin).map(|removal| removal.argv)
    }

    /// The one thing that must be true of every recipe: the package is its own
    /// argument, behind `--`, and nothing is ever a shell string.
    #[test]
    fn every_removal_passes_the_package_as_its_own_argument() {
        let origins = [
            Origin::Flatpak {
                id: APP_ID.into(),
                scope: Scope::User,
            },
            Origin::Snap {
                name: PACKAGE.into(),
            },
            Origin::System {
                manager: Manager::Pacman,
                package: PACKAGE.into(),
            },
            Origin::System {
                manager: Manager::Dpkg,
                package: PACKAGE.into(),
            },
        ];
        for origin in origins {
            let Some(argv) = argv(&origin) else {
                // The tool for this one is not installed here, which is the
                // other half of the contract and is checked below.
                continue;
            };
            let last = argv.last().expect("a removal names something");
            assert_eq!(Some(last.as_str()), origin.package());
            assert_eq!(
                argv[argv.len() - 2],
                "--",
                "{argv:?} does not fence the package off from the flags"
            );
            assert!(
                argv.iter().all(|part| !part.contains(' ')),
                "{argv:?} looks like a command line rather than an argv"
            );
        }
    }

    /// A user flatpak is the one removal that asks nobody for anything; a
    /// system one is still not sudo's business, because flatpak has polkit for
    /// it.
    #[test]
    fn a_flatpak_never_needs_root() {
        for scope in [Scope::User, Scope::System] {
            let origin = Origin::Flatpak {
                id: APP_ID.into(),
                scope,
            };
            let Some(removal) = removal(&origin) else {
                continue;
            };
            assert!(!removal.root);
            assert_eq!(authority(&removal), Authority::NotNeeded);
            let wanted = match scope {
                Scope::User => "--user",
                Scope::System => "--system",
            };
            assert!(
                removal.argv.iter().any(|part| part == wanted),
                "{removal:?}"
            );
        }
    }

    /// A snap needs root even though it installed itself without a package
    /// manager — this is the case the shell would get wrong by assuming that
    /// "not a system package" means "no authority needed".
    #[test]
    fn a_snap_needs_root() {
        let origin = Origin::Snap {
            name: PACKAGE.into(),
        };
        if let Some(removal) = removal(&origin) {
            assert!(removal.root);
        }
    }

    /// Nothing is offered for an application the shell cannot place, and that
    /// is the point: a plan with no removal in it is what puts up the panel
    /// saying so.
    #[test]
    fn an_unknown_origin_has_no_removal_and_nothing_to_authorise() {
        assert_eq!(removal(&Origin::Unknown), None);
        let plan = plan(&Origin::Unknown);
        assert_eq!(plan.removal, None);
        assert_eq!(plan.authority, Authority::NotNeeded);
    }

    /// A removal naming a program this machine does not have is not offered at
    /// all. Finding out after the user has answered a question about destroying
    /// something is the worst possible moment.
    #[test]
    fn a_removal_is_never_offered_for_a_tool_that_is_not_here() {
        for origin in [
            Origin::Flatpak {
                id: APP_ID.into(),
                scope: Scope::User,
            },
            Origin::Snap {
                name: PACKAGE.into(),
            },
            Origin::System {
                manager: Manager::Pacman,
                package: PACKAGE.into(),
            },
            Origin::System {
                manager: Manager::Dpkg,
                package: PACKAGE.into(),
            },
            Origin::System {
                manager: Manager::Rpm,
                package: PACKAGE.into(),
            },
        ] {
            if let Some(removal) = removal(&origin) {
                assert!(
                    installed(&removal.argv[0]),
                    "{} is not on this machine",
                    removal.argv[0]
                );
            }
        }
        assert!(!installed("lxb-no-such-program-anywhere"));
    }

    #[test]
    fn a_failure_is_reduced_to_one_readable_line() {
        assert_eq!(
            complaint(b"error: target not found: nothing\n", &[]),
            "target not found: nothing"
        );
        // stdout is the fallback for a tool that says nothing on stderr.
        assert_eq!(complaint(b"", b"\n\nnothing to do\n"), "nothing to do");
        assert_eq!(complaint(b"", &[]), "the removal failed and said nothing");
        // And a wall of text becomes one line rather than running off the
        // panel.
        let long = "x".repeat(400);
        let cut = complaint(long.as_bytes(), &[]);
        assert!(cut.chars().count() <= 121 && cut.ends_with('…'), "{cut}");
    }
}
