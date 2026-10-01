//! Settings > Power as the machine's settings rather than one account's.
//!
//! How long a console waits before its screen dims, goes dark and sleeps, and
//! what its power button does, are facts about the console. Everybody who
//! signs into it gets the same ones, and so does the login screen in front of
//! all of them — which is the screen a handheld spends most of a night on if
//! nobody signs in, and the one place a power button pressed by mistake used
//! to switch the whole machine off. So the page writes the machine's settings,
//! in two places, and reads them back from the first:
//!
//! * [`FILE`], `/etc/lxb/power.toml`: every wait, the button and the battery
//!   saver, in the spelling `shell.toml` would have used. Every LineXinBar
//!   session on the machine reads it, and so does the login screen (CEDM),
//!   which cannot read anybody's home.
//! * [`LOGIND_DROP_IN`]: the power button, as the login manager's own
//!   `HandlePowerKey`. That is the system's setting for what the button does
//!   where no session has taken it — the login screen, a text console, the
//!   moment between two sessions — and logind is re-read so it applies at once.
//!   A LineXinBar session takes the button for itself while it runs (see
//!   [`crate::power_bus`]) and answers it the same way, so a press means one
//!   thing everywhere on the machine.
//!
//! logind's own idle action is deliberately left alone. It has one wait where
//! this page has two (the battery's and the mains'), and it cannot tell a
//! download or a film from a person having gone away; the session and the
//! login screen each carry the waits out themselves, from this file.
//!
//! ## Who may write it
//!
//! Both files are root's, so the page asks polkit to run this same program as
//! root with [`APPLY_FLAG`] and six values — the pattern of
//! [`crate::locale::apply_as_root`]. The action is allowed without a password
//! to whoever is at the machine, as sleeping and switching it off already are:
//! a console that asked for a password to change how soon its screen dims
//! would be asking for one on the page people visit most. What bounds it is
//! what the root half can do — six validated values, two files of fixed shape,
//! and nothing else.
//!
//! ## Sleeping by hand
//!
//! The second root half, [`SLEEP_FLAG`], is how a sleep somebody *asked for* —
//! the power menu, the power button — goes through a program that holds sleep
//! off. A game may keep the machine awake while nobody touches it; it may not
//! keep awake a machine somebody told to sleep. Since systemd 257 logind holds
//! even the account's own programs' locks against the account itself, and
//! setting them aside is a polkit action that asks for an administrator's
//! password. This does it without one, but only for locks the asking account's
//! own programs hold: a lock of the system's — a firmware update, another
//! account's work — is still honoured, and the person is told the machine will
//! sleep once that has finished.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::settings::{PowerButton, PowerSettings};

/// The machine's power settings, readable by every account and the login
/// screen.
pub const FILE: &str = "/etc/lxb/power.toml";

/// The login manager's half: what the power button does where no session has
/// taken it.
pub const LOGIND_DROP_IN: &str = "/etc/systemd/logind.conf.d/60-lxb-power.conf";

/// The flags the installed polkit actions are bound to.
pub const APPLY_FLAG: &str = "--apply-power";
pub const SLEEP_FLAG: &str = "--sleep-now";

/// The longest wait the root half accepts: a week. Nothing on the page offers
/// more than three hours; this is only a bound on what can be written.
const LONGEST_WAIT: u32 = 7 * 24 * 3600;

/// The exit status [`sleep_now_as_root`] uses for "not done, because a lock
/// the system holds is still in force" — the one refusal the shell says
/// something about.
pub const HELD_BY_THE_SYSTEM: i32 = 4;

/// How often a running session looks at [`FILE`] again, for a change another
/// account made. One `stat` a minute.
const LOOK_AGAIN: Duration = Duration::from_secs(60);

/// The file's keys: `shell.toml`'s spelling for the same settings, so the page
/// that writes them and the login screen that reads them name each one alike.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
struct Written {
    dim_screen_after: Option<u32>,
    screen_off_after: Option<u32>,
    sleep_on_battery_after: Option<u32>,
    sleep_plugged_in_after: Option<u32>,
    power_button: Option<String>,
    battery_saver: Option<bool>,
}

/// What the machine is set to: [`FILE`], with anything it does not say — or a
/// machine that has no file yet — at [`PowerSettings::DEFAULT`].
pub fn read() -> PowerSettings {
    read_from(Path::new(FILE))
}

pub fn read_from(path: &Path) -> PowerSettings {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => PowerSettings::DEFAULT,
        Err(err) => {
            tracing::warn!(%err, file = %path.display(), "the machine's power settings could not be read");
            PowerSettings::DEFAULT
        }
    }
}

/// Settings from the file's text. A key that is missing or that says
/// something this shell has no answer for is the default for that key alone.
fn parse(text: &str) -> PowerSettings {
    let written: Written = match toml::from_str(text) {
        Ok(written) => written,
        Err(err) => {
            tracing::warn!(%err, "the machine's power settings are not a settings file");
            return PowerSettings::DEFAULT;
        }
    };
    let mut settings = PowerSettings::DEFAULT;
    let wait = |seconds: Option<u32>, default: u32| {
        seconds.filter(|s| *s <= LONGEST_WAIT).unwrap_or(default)
    };
    settings.dim_after = wait(written.dim_screen_after, settings.dim_after);
    settings.screen_off_after = wait(written.screen_off_after, settings.screen_off_after);
    settings.sleep_on_battery = wait(written.sleep_on_battery_after, settings.sleep_on_battery);
    settings.sleep_plugged_in = wait(written.sleep_plugged_in_after, settings.sleep_plugged_in);
    if let Some(word) = written.power_button.as_deref() {
        match PowerButton::from_key(word) {
            Some(button) => settings.button = button,
            None => tracing::warn!(word, "no power button answer by that name"),
        }
    }
    if let Some(on) = written.battery_saver {
        settings.battery_saver = on;
    }
    settings
}

/// The file's whole text for `settings`.
fn document(settings: PowerSettings) -> String {
    let written = Written {
        dim_screen_after: Some(settings.dim_after),
        screen_off_after: Some(settings.screen_off_after),
        sleep_on_battery_after: Some(settings.sleep_on_battery),
        sleep_plugged_in_after: Some(settings.sleep_plugged_in),
        power_button: Some(settings.button.key().to_string()),
        battery_saver: Some(settings.battery_saver),
    };
    let body = toml::to_string(&written).expect("six plain values always serialise");
    format!(
        "# This device's power settings, as LineXinBar's Settings > Power chose them.\n\
         # Every LineXinBar session on the device and its login screen read this file.\n\
         # It is written whole each time a setting on that page is changed.\n\
         #\n\
         # The waits are in seconds, and 0 is never. power-button is one of sleep,\n\
         # hibernate, power-off, menu or nothing.\n\
         \n{body}"
    )
}

/// What logind is told the power button does, for the page's answer. The two
/// answers logind has no word for — the menu, which only a screen can show,
/// and nothing — are both `ignore`: the session and the login screen draw the
/// menu themselves, and a text console has none to draw.
pub fn logind_action(button: PowerButton) -> &'static str {
    match button {
        PowerButton::Sleep => "suspend",
        PowerButton::Hibernate => "hibernate",
        PowerButton::PowerOff => "poweroff",
        PowerButton::Menu | PowerButton::Nothing => "ignore",
    }
}

fn logind_document(button: PowerButton) -> String {
    format!(
        "# Written by LineXinBar's Settings > Power: what the power button does where\n\
         # no session answers it itself, such as the login screen or a text console.\n\
         # Replaced whole each time that page's Power button row is changed.\n\
         [Login]\n\
         HandlePowerKey={}\n",
        logind_action(button)
    )
}

/// The six values the root half is handed, in order.
fn arguments(settings: PowerSettings) -> [String; 6] {
    [
        settings.dim_after.to_string(),
        settings.screen_off_after.to_string(),
        settings.sleep_on_battery.to_string(),
        settings.sleep_plugged_in.to_string(),
        settings.button.key().to_string(),
        if settings.battery_saver { "on" } else { "off" }.to_string(),
    ]
}

/// Those six values back into settings, refusing anything the page could not
/// have asked for.
fn from_arguments(values: &[String]) -> anyhow::Result<PowerSettings> {
    let [dim, off, battery, plugged, button, saver] = values else {
        anyhow::bail!("six values are wanted, not {}", values.len());
    };
    let wait = |value: &str| -> anyhow::Result<u32> {
        anyhow::ensure!(
            !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
            "{value:?} is not a number of seconds"
        );
        let seconds: u32 = value.parse()?;
        anyhow::ensure!(
            seconds <= LONGEST_WAIT,
            "{seconds} seconds is longer than a week"
        );
        Ok(seconds)
    };
    Ok(PowerSettings {
        dim_after: wait(dim)?,
        screen_off_after: wait(off)?,
        sleep_on_battery: wait(battery)?,
        sleep_plugged_in: wait(plugged)?,
        button: PowerButton::from_key(button)
            .ok_or_else(|| anyhow::anyhow!("{button:?} is not an answer for the power button"))?,
        battery_saver: match saver.as_str() {
            "on" => true,
            "off" => false,
            other => anyhow::bail!("{other:?} is neither on nor off"),
        },
    })
}

// --- the root halves -------------------------------------------------------

/// The privileged half of Settings > Power: write both files and have logind
/// read its own again.
///
/// Started by polkit, from [`adopt`], and by nothing else. It can write
/// exactly two files, each whole, from six values it has checked — which is
/// the whole of what makes it safe to allow without a password.
pub fn apply_as_root(values: &[String]) -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("this is polkit's half of Settings > Power and only runs as root");
    }
    let settings = from_arguments(values)?;
    if write_in(Path::new("/"), settings)? {
        reload_logind();
    }
    Ok(())
}

/// Write both files under `root` — `/` for the machine, a scratch directory
/// for a test. Returns whether logind's file changed and wants re-reading.
fn write_in(root: &Path, settings: PowerSettings) -> anyhow::Result<bool> {
    let under = |path: &str| root.join(path.trim_start_matches('/'));
    write_whole(&under(FILE), &document(settings))?;
    let logind = under(LOGIND_DROP_IN);
    let wanted = logind_document(settings.button);
    if std::fs::read_to_string(&logind).is_ok_and(|already| already == wanted) {
        return Ok(false);
    }
    write_whole(&logind, &wanted)?;
    Ok(true)
}

/// Replace a file whole: a copy beside it, flushed, then renamed over it, so a
/// reader never finds half of one.
fn write_whole(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let directory = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(directory)?;
    let mut beside: PathBuf = directory.to_path_buf();
    beside.push(format!(
        ".{}.new",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("power")
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&beside)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::set_permissions(&beside, std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
    std::fs::rename(&beside, path)?;
    Ok(())
}

/// Have logind read its configuration again, so the button's new answer holds
/// from now rather than from the next start. A logind too old to be reloaded
/// takes it at the next start, which the journal says.
fn reload_logind() {
    let reloaded = std::process::Command::new("systemctl")
        .args(["reload", "systemd-logind.service"])
        .stdin(std::process::Stdio::null())
        .status();
    match reloaded {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!(
            "the login manager was not reloaded ({status}); the power button's new answer \
             applies from the next start"
        ),
        Err(err) => eprintln!("systemctl could not be started: {err}"),
    }
}

/// logind's flag for setting block locks aside. See the module's notes.
const SKIP_INHIBITORS: u64 = 1 << 4;

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

/// The privileged half of a sleep somebody asked for, past their own
/// programs' locks.
///
/// Started by polkit, from [`ask_root_to_sleep`], with `suspend` or
/// `hibernate`. It sets aside a lock only where every lock that would stop the
/// sleep belongs to the account that asked (`PKEXEC_UID`, which pkexec sets
/// and nobody else can): a lock of the system's or of another account's ends
/// this with [`HELD_BY_THE_SYSTEM`] and nothing done.
pub fn sleep_now_as_root(how: &str) -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("this is polkit's half of sleeping by hand and only runs as root");
    }
    let plain = match how {
        "suspend" => "Suspend",
        "hibernate" => "Hibernate",
        other => anyhow::bail!("{other:?} is neither suspend nor hibernate"),
    };
    let asker: u32 = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|uid| uid.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("nobody is named as having asked"))?;
    let bus = zbus::blocking::Connection::system()?;
    let locks = blocking_locks(&bus)?;
    if let Some(theirs) = locks.iter().find(|lock| lock.uid != asker) {
        eprintln!(
            "{} ({}) holds sleep off for uid {}: {}",
            theirs.who, theirs.what, theirs.uid, theirs.why
        );
        std::process::exit(HELD_BY_THE_SYSTEM);
    }
    let with_flags = format!("{plain}WithFlags");
    match bus.call_method(
        Some(LOGIND),
        LOGIND_PATH,
        Some(LOGIND_MANAGER),
        with_flags.as_str(),
        &(SKIP_INHIBITORS,),
    ) {
        Ok(_) => Ok(()),
        // A logind from before the flag existed, which is also a logind that
        // never held root to anybody's lock.
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod"
                || name.as_str() == "org.freedesktop.DBus.Error.InvalidArgs" =>
        {
            bus.call_method(
                Some(LOGIND),
                LOGIND_PATH,
                Some(LOGIND_MANAGER),
                plain,
                &(false,),
            )?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

/// One of logind's locks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    pub what: String,
    pub who: String,
    pub why: String,
    pub uid: u32,
}

/// The locks that would stop a sleep: `block` mode, covering `sleep`.
pub fn blocking_locks(bus: &zbus::blocking::Connection) -> anyhow::Result<Vec<Lock>> {
    let reply = bus.call_method(
        Some(LOGIND),
        LOGIND_PATH,
        Some(LOGIND_MANAGER),
        "ListInhibitors",
        &(),
    )?;
    let all: Vec<(String, String, String, String, u32, u32)> = reply.body().deserialize()?;
    Ok(all
        .into_iter()
        .filter(|(what, _, _, mode, _, _)| stops_sleep(what, mode))
        .map(|(what, who, why, _, uid, _)| Lock {
            what,
            who,
            why,
            uid,
        })
        .collect())
}

/// Whether a lock of this kind and mode stops a sleep somebody asked for.
/// `delay` only makes sleep wait for its holder, and the `handle-*` kinds are
/// about keys, not sleep.
fn stops_sleep(what: &str, mode: &str) -> bool {
    mode.starts_with("block") && what.split(':').any(|kind| kind == "sleep")
}

// --- the unprivileged half -------------------------------------------------

/// Where the machine's settings stand, for the page to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The machine has them.
    Saved,
    /// The machine could not be given them, so this session alone uses them.
    SessionOnly,
}

struct State {
    standing: Option<Standing>,
    busy: bool,
    next: Option<PowerSettings>,
    /// When the file was last read, and what it said then.
    looked: Option<Instant>,
    modified: Option<SystemTime>,
}

static STATE: std::sync::Mutex<State> = std::sync::Mutex::new(State {
    standing: None,
    busy: false,
    next: None,
    looked: None,
    modified: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether this session is on the machine's own displays, and so may change
/// the machine's settings. A session inside another desktop keeps the page's
/// answers to itself: the machine it runs on is somebody's desktop, not this
/// console.
pub fn owns_the_machine() -> bool {
    std::env::var("LXB_SESSION_BACKEND").as_deref() == Ok("drm")
}

/// Give the machine `settings`, from the press that changed them. Returns at
/// once; the page's note moves when there is an answer.
pub fn adopt(settings: PowerSettings) {
    if !owns_the_machine() {
        tracing::info!(
            ?settings,
            "a session inside another desktop keeps the power settings to itself"
        );
        return;
    }
    let mut guard = state();
    if guard.busy {
        guard.next = Some(settings);
        return;
    }
    guard.busy = true;
    drop(guard);
    let spawned = std::thread::Builder::new()
        .name("lxb-power-settings".into())
        .spawn(move || {
            let mut settings = settings;
            loop {
                let standing = match ask_root_to_apply(settings) {
                    Ok(()) => Standing::Saved,
                    Err(why) => {
                        tracing::warn!(why, "the machine's power settings were not changed");
                        Standing::SessionOnly
                    }
                };
                let mut state = state();
                state.standing = Some(standing);
                // What was just written is what this session has, so it is not
                // read back as somebody else's change.
                state.modified = modified(Path::new(FILE));
                match state.next.take() {
                    Some(again) => settings = again,
                    None => {
                        state.busy = false;
                        return;
                    }
                }
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "no thread to change the power settings on");
        state().busy = false;
    }
}

/// How the last change stands, if it said anything worth a note.
pub fn standing() -> Option<Standing> {
    state().standing
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Read the machine's settings, noting when the file was last changed.
pub fn prime() -> PowerSettings {
    let mut state = state();
    state.looked = Some(Instant::now());
    state.modified = modified(Path::new(FILE));
    drop(state);
    read()
}

/// The machine's settings again, if another account has changed them since
/// they were last read — looked at once a minute.
pub fn changed_elsewhere(now: Instant) -> Option<PowerSettings> {
    let mut state = state();
    if state.busy
        || state
            .looked
            .is_some_and(|looked| now.saturating_duration_since(looked) < LOOK_AGAIN)
    {
        return None;
    }
    state.looked = Some(now);
    let modified = modified(Path::new(FILE));
    if modified == state.modified {
        return None;
    }
    state.modified = modified;
    drop(state);
    tracing::info!("the machine's power settings were changed elsewhere");
    Some(read())
}

/// Ask polkit to run this program as root to write both files.
fn ask_root_to_apply(settings: PowerSettings) -> Result<(), String> {
    let pkexec = crate::locale::pkexec().ok_or("no pkexec on this machine")?;
    let exe = crate::locale::this_program().map_err(|err| err.to_string())?;
    let answered = std::process::Command::new(pkexec)
        .arg("--disable-internal-agent")
        .arg(&exe)
        .arg(APPLY_FLAG)
        .args(arguments(settings))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| err.to_string())?;
    match answered.status.code() {
        Some(0) => Ok(()),
        _ => Err(String::from_utf8_lossy(&answered.stderr).trim().to_string()),
    }
}

/// How a sleep asked of root went.
#[derive(Debug, PartialEq, Eq)]
pub enum RootSleep {
    Done,
    /// A lock of the system's or another account's is in force.
    HeldByTheSystem,
    Failed(String),
}

/// Ask polkit to run this program as root to sleep past the account's own
/// programs' locks. `how` is `suspend` or `hibernate`.
pub fn ask_root_to_sleep(how: &'static str) -> RootSleep {
    let Some(pkexec) = crate::locale::pkexec() else {
        return RootSleep::Failed("no pkexec on this machine".into());
    };
    let exe = match crate::locale::this_program() {
        Ok(exe) => exe,
        Err(err) => return RootSleep::Failed(err.to_string()),
    };
    let answered = std::process::Command::new(pkexec)
        .arg("--disable-internal-agent")
        .arg(&exe)
        .arg(SLEEP_FLAG)
        .arg(how)
        .stdin(std::process::Stdio::null())
        .output();
    match answered {
        Ok(answered) => match answered.status.code() {
            Some(0) => RootSleep::Done,
            Some(HELD_BY_THE_SYSTEM) => {
                tracing::info!(
                    said = %String::from_utf8_lossy(&answered.stderr).trim(),
                    "sleep is held off by a lock of the system's"
                );
                RootSleep::HeldByTheSystem
            }
            _ => RootSleep::Failed(String::from_utf8_lossy(&answered.stderr).trim().to_string()),
        },
        Err(err) => RootSleep::Failed(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lxb-machine-power-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// No file is the console's defaults, and a file that says only some things
    /// changes only those.
    #[test]
    fn a_machine_with_no_file_has_the_defaults_and_a_file_changes_what_it_says() {
        let dir = scratch("read");
        assert_eq!(read_from(&dir.join("power.toml")), PowerSettings::DEFAULT);
        std::fs::write(
            dir.join("power.toml"),
            "screen-off-after = 60\npower-button = \"power-off\"\n",
        )
        .unwrap();
        let read = read_from(&dir.join("power.toml"));
        assert_eq!(read.screen_off_after, 60);
        assert_eq!(read.button, PowerButton::PowerOff);
        assert_eq!(read.dim_after, PowerSettings::DEFAULT.dim_after);
        assert_eq!(
            parse("power-button = \"launch\"\n").button,
            PowerButton::Sleep
        );
        assert_eq!(parse("not toml at all ["), PowerSettings::DEFAULT);
        assert_eq!(
            parse("dim-screen-after = 999999999\n").dim_after,
            PowerSettings::DEFAULT.dim_after,
            "longer than a week is not a wait"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What is written reads back as the same settings, and every answer the
    /// button has survives the trip through the root half's arguments.
    #[test]
    fn settings_survive_the_file_and_the_arguments() {
        for button in PowerButton::ALL {
            let settings = PowerSettings {
                dim_after: 30,
                screen_off_after: 0,
                sleep_on_battery: 600,
                sleep_plugged_in: 7200,
                button,
                battery_saver: false,
            };
            assert_eq!(parse(&document(settings)), settings);
            assert_eq!(from_arguments(&arguments(settings)).unwrap(), settings);
        }
    }

    /// The root half refuses anything the page could not have asked for.
    #[test]
    fn the_root_half_takes_only_what_the_page_could_ask_for() {
        let good = arguments(PowerSettings::DEFAULT).to_vec();
        assert!(from_arguments(&good).is_ok());
        let with = |index: usize, value: &str| {
            let mut values = good.clone();
            values[index] = value.to_string();
            from_arguments(&values)
        };
        assert!(with(0, "-5").is_err());
        assert!(with(0, "1e3").is_err());
        assert!(with(0, "").is_err());
        assert!(with(1, &(LONGEST_WAIT + 1).to_string()).is_err());
        assert!(with(4, "reboot").is_err());
        assert!(with(4, "sleep\nHandleLidSwitch=ignore").is_err());
        assert!(with(5, "yes").is_err());
        assert!(from_arguments(&good[..5]).is_err());
    }

    /// Both files are written under the root given, whole and readable by
    /// everybody, and logind's is reported as changed only when it did.
    #[test]
    fn both_files_are_written_and_logind_is_reloaded_only_for_a_change() {
        let root = scratch("write");
        let mut settings = PowerSettings::DEFAULT;
        assert!(
            write_in(&root, settings).unwrap(),
            "a first write changes logind's"
        );
        let power = root.join("etc/lxb/power.toml");
        let logind = root.join("etc/systemd/logind.conf.d/60-lxb-power.conf");
        assert_eq!(read_from(&power), settings);
        let text = std::fs::read_to_string(&logind).unwrap();
        assert!(text.contains("[Login]\nHandlePowerKey=suspend\n"), "{text}");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&power).unwrap().permissions().mode() & 0o777,
            0o644
        );

        settings.dim_after = 60;
        assert!(
            !write_in(&root, settings).unwrap(),
            "the button did not change"
        );
        assert_eq!(read_from(&power).dim_after, 60);

        settings.button = PowerButton::Menu;
        assert!(write_in(&root, settings).unwrap());
        assert!(std::fs::read_to_string(&logind)
            .unwrap()
            .contains("HandlePowerKey=ignore"));
        assert!(
            std::fs::read_dir(root.join("etc/lxb"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".new")),
            "nothing is left beside the file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each answer the button has is one logind understands.
    #[test]
    fn every_answer_is_a_word_logind_knows() {
        for button in PowerButton::ALL {
            assert!(matches!(
                logind_action(button),
                "suspend" | "hibernate" | "poweroff" | "ignore"
            ));
        }
    }

    /// Only a block lock that covers sleep stops a sleep somebody asked for.
    #[test]
    fn only_a_block_on_sleep_stops_a_sleep() {
        assert!(stops_sleep("sleep", "block"));
        assert!(stops_sleep("shutdown:sleep:idle", "block"));
        assert!(stops_sleep("sleep", "block-weak"));
        assert!(!stops_sleep("sleep", "delay"));
        assert!(!stops_sleep("idle", "block"));
        assert!(!stops_sleep("handle-power-key", "block"));
    }

    /// Neither root half runs as anybody but root.
    #[test]
    fn the_root_halves_refuse_an_ordinary_account() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert!(apply_as_root(&arguments(PowerSettings::DEFAULT)).is_err());
        assert!(sleep_now_as_root("suspend").is_err());
    }
}
