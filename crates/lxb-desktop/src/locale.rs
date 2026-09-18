//! The system language, kept the same as the shell's.
//!
//! Settings > Language names a language the *machine* speaks, not a private
//! choice of this shell's. The person who picks Polski there expects the
//! browser, the file dialog and the game they open next to come up in Polish,
//! which is what choosing a language on a console does — and this shell is
//! the session's desktop, so a setting the operating system has is the
//! operating system's to keep, not a copy of it here. Three things carry a
//! choice:
//!
//! 1. **`/etc/locale.conf`**, through `systemd-localed`
//!    (`org.freedesktop.locale1`) on the system bus, which owns the system
//!    locale. Its `SetLocale` writes the file, tells the service manager, and
//!    on a distribution built round `locale-gen` generates a locale that is
//!    not there yet. polkit answers through this shell's own agent
//!    ([`crate::polkit`]), so the password question is the shell's panel.
//!    Only `LANG` is sent: localed merges it with what the file has, so an
//!    `LC_TIME` somebody set apart from their language stays set apart.
//! 2. **This process's environment**, which every program the shell starts
//!    inherits — Valve's client, a game, a helper, a terminal. `LANG` is
//!    written. `LC_ALL`, `LC_MESSAGES` and `LANGUAGE` are written only where
//!    the session already had them, because each outranks `LANG` when it is
//!    present and would keep the old language if left alone.
//! 3. **The session bus's activation environment** and the systemd user
//!    manager's, best effort, so a program D-Bus starts on somebody's behalf
//!    follows too.
//! 4. **The machine's own environment files** — `/etc/environment`, which PAM
//!    reads into every login session, and `/etc/environment.d/*.conf`. A
//!    `LANG` or an `LC_ALL` written in one of those is read *after*
//!    `/etc/locale.conf` and outranks it, so a machine that has one goes on
//!    telling the login screen and every system service the old language
//!    however carefully the system locale was set. Where one of them names
//!    another language its value is changed to the chosen one, by
//!    [`apply_as_root`], which polkit starts because those files are root's.
//!    The session's own `~/.config/environment.d/*.conf` needs nobody's leave
//!    and is changed in place.
//!
//! What the fourth does is deliberately small. No line is added and none is
//! removed: a variable the machine already had is given the language the
//! person just chose, and everything else in the file — the other variables,
//! the comments, the spacing, the quotes — is written back exactly as it was
//! found. The shell does not own those files. It is changing the value of a
//! setting the machine already had, which is the whole of what "change the
//! system language" means, and it is the reason that press is worth a
//! password.
//!
//! Nothing already running changes — a program reads its locale once, at its
//! start — so what the row says is "from now on", and a restart is what
//! finishes the job for the login screen and everything started before the
//! press. And nothing is exported that glibc could not load: a locale that is
//! not installed would leave every program the shell opened in the C locale
//! and warning about it, which is worse than the language it had.
//!
//! A shell nobody has chosen a language on ([`Language::System`]) touches
//! none of this. It follows the session, which is what the row shows.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::i18n::{self, Language};

const LOCALED: &str = "org.freedesktop.locale1";
const LOCALED_PATH: &str = "/org/freedesktop/locale1";
const LOCALED_IFACE: &str = "org.freedesktop.locale1";

/// The locale each language is given when the session was not already in
/// one of its own: the one every installer offers first.
///
/// The two Englishes get the two countries they are named after, and that is
/// the whole of what makes the choice reach anything that is not this shell.
/// `LC_TIME` is merged out of `LANG` by everything that reads a locale, so a
/// browser, a spreadsheet and `date` in a terminal all write the American
/// order for `en_US.UTF-8` and the British one for `en_GB.UTF-8` without
/// being told twice.
const DEFAULT_LOCALE: [(Language, &str); 10] = [
    (Language::British, "en_GB.UTF-8"),
    (Language::American, "en_US.UTF-8"),
    (Language::French, "fr_FR.UTF-8"),
    (Language::Spanish, "es_ES.UTF-8"),
    (Language::Polish, "pl_PL.UTF-8"),
    (Language::German, "de_DE.UTF-8"),
    (Language::Hindi, "hi_IN.UTF-8"),
    (Language::Portuguese, "pt_BR.UTF-8"),
    (Language::Russian, "ru_RU.UTF-8"),
    (Language::Chinese, "zh_CN.UTF-8"),
];

/// The variables that decide which language a program speaks, in the order
/// glibc reads them. `LANG` is always written; the rest only where present.
const OVERRIDES: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANGUAGE"];

/// Where the choice stands, for the note under the row that is in force.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The shell and the system speak the language chosen.
    Spoken,
    /// localed is being asked. The answer arrives on a later frame.
    Changing,
    /// The shell and what it opens speak it, and there is no locale service
    /// on this machine to write it down for the next start.
    NoService,
    /// The system's own setting was refused, in the daemon's words. Whether
    /// the session was still told is `session_follows`: it is, unless the
    /// refusal was that the locale is not installed.
    Refused { why: String, session_follows: bool },
    /// The system took it: the locale service has it for the next start, and
    /// anything on this machine that named another language has been changed
    /// to this one.
    ///
    /// Only ever reached from a press. The note it carries asks for a restart,
    /// because the login screen and every service started before the press
    /// read their language once and are still holding the old one — and a
    /// restart is worth asking for only where somebody has just changed
    /// something, so [`prime`] never ends here.
    Changed,
    /// The shell and what it opens speak it, and something on the machine
    /// still names another language: the change to the files that would put
    /// that right was not authorized, or could not be made. Only ever reached
    /// from a press, and the note it carries says to choose the language
    /// again.
    NotEverywhere,
}

struct State {
    standing: Option<Standing>,
    /// How many times the standing has changed. The frame loop compares it
    /// rather than the standing, which is the same on nearly every frame.
    published: u64,
    /// A worker is on the bus. A press made meanwhile is kept in `next` and
    /// carried out by the same worker when it is done, so two presses never
    /// race each other to localed.
    busy: bool,
    next: Option<Language>,
}

static STATE: Mutex<State> = Mutex::new(State {
    standing: None,
    published: 0,
    busy: false,
    next: None,
});

/// The files a machine's environment is built from, in the order a person
/// would look in them. `/etc/environment` is PAM's, read into every login
/// session — the login screen's among them; `/etc/environment.d/*.conf` is
/// systemd's newer spelling of the same thing. Both are root's.
const ENVIRONMENT_FILES: [&str; 1] = ["/etc/environment"];
const ENVIRONMENT_DIRECTORY: &str = "/etc/environment.d";

/// The session's own half of the same thing, which the shell may write itself.
const SESSION_ENVIRONMENT_DIRECTORY: &str = "environment.d";

/// The variables that decide which language a program speaks, in the order
/// glibc reads them. The first three outrank the `LANG` the locale service
/// writes to `/etc/locale.conf`; the fourth *is* `LANG`, written a second time
/// in a file that is read later, so it wins over that one too.
const DECIDE_THE_LANGUAGE: [&str; 4] = ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"];

/// The word polkit's action is bound to, and the flag the shell is started
/// with to be the privileged half of a language change. They are the same
/// string in three places — here, the policy file the package installs, and
/// the command line — so it is written once.
pub const APPLY_FLAG: &str = "--apply-language";

/// The locale the session started in, raw — `en_GB.UTF-8` rather than the
/// `en-GB` the catalogs are chosen by — so that English chosen on a British
/// machine keeps it British. Read once, before anything below rewrites it.
static STARTED_IN: LazyLock<Option<String>> = LazyLock::new(|| {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Make the session speak what the settings file says, at startup.
///
/// Called before any other thread exists, which is the one time a variable
/// can be *added* to the environment safely: glibc's `setenv` grows the
/// `environ` array for a new name and frees the old one, and a thread walking
/// it through `getenv` at that moment reads freed memory. Rewriting a name
/// that is already there swaps one pointer and frees nothing, which is why
/// [`export`] may run later from a worker — and why `LANG` is planted here if
/// the session came without one, so that a later change is always a rewrite.
///
/// No bus call is made: that is a press's to make, with a password panel if
/// the machine wants one, and not something a shell does to a machine every
/// time it starts.
pub fn prime() {
    LazyLock::force(&STARTED_IN);
    // Nothing here ever ends in [`Standing::Pinned`], which is the note asking
    // for a restart. A session that has just started *is* the restart: the
    // language was not changed while anybody was looking at it, nothing is
    // waiting to be applied, and a row that opened saying "a restart may be
    // needed" would be asking for one to no end. What the machine does with
    // the language is still written to the journal at startup — see
    // [`pinned`], which the press path asks.
    let chosen = i18n::preference();
    let locale = locale_for(chosen);
    let speaks = chosen == Language::System || session_speaks(chosen);
    let have_it = installed(locale);
    if !speaks && have_it {
        let written = export(locale, chosen.gettext_name());
        tracing::info!(locale, ?written, "the session follows the shell's language");
    }
    if !speaks && !have_it {
        tracing::warn!(
            locale,
            "the shell's language names a locale that is not installed"
        );
    }
    // Said to the journal whether or not anything is drawn about it: somebody
    // asking why the login screen is in the other language is reading this,
    // and a session that has just started says nothing on the row. Nothing is
    // written here either — a file of root's is changed from a press, with a
    // password panel in front of it, and never because a shell started.
    if chosen != Language::System {
        for repair in repairs_in(&machine_files(), locale) {
            tracing::warn!(
                file = %repair.file.display(),
                variables = ?repair.variables,
                locale,
                "the machine's environment names another language than the shell's; \
                 choosing the language again in Settings puts it right"
            );
        }
    }
    let standing = standing_at_startup(chosen, speaks, have_it);
    if std::env::var_os("LANG").is_none() {
        // Planted so that a later rewrite finds it — see above. `LC_ALL`, when
        // set, outranks it in every program, and `C` is what glibc reads an
        // absent `LANG` as; either way nothing a child sees changes.
        let seed = std::env::var("LC_ALL").unwrap_or_else(|_| "C".to_string());
        // SAFETY: no other thread exists yet; see the doc comment.
        unsafe { std::env::set_var("LANG", seed) };
    }
    let mut state = state();
    state.standing = Some(standing);
    state.published += 1;
}

/// Where a session that has only just started stands.
///
/// Never [`Standing::Pinned`], whatever the machine says: that note asks for a
/// restart, and a session that has just started *is* the restart. Nothing has
/// been changed while anybody was looking at it, so there is nothing waiting
/// to be applied and nothing to ask for. Pure, so the rule can be checked
/// without a machine to read.
fn standing_at_startup(chosen: Language, session_speaks: bool, locale_installed: bool) -> Standing {
    match chosen {
        Language::System => Standing::Spoken,
        _ if session_speaks || locale_installed => Standing::Spoken,
        chosen => Standing::Refused {
            why: crate::message!("language-locale-not-installed", "locale" => locale_for(chosen)),
            session_follows: false,
        },
    }
}

/// Make the system speak `language`, from the press that chose it.
///
/// Returns at once; the work is on a thread, because localed may put a
/// password panel up and then generate a locale, and neither is a thing to
/// hold a frame for. [`published`] moves when there is an answer.
pub fn adopt(language: Language) {
    let mut state = state();
    state.standing = Some(Standing::Changing);
    state.published += 1;
    if state.busy {
        state.next = Some(language);
        return;
    }
    state.busy = true;
    drop(state);
    std::thread::Builder::new()
        .name("locale".into())
        .spawn(move || work(language))
        .expect("a thread for one bus call");
}

/// How the last choice stands. `None` before [`prime`].
pub fn standing() -> Option<Standing> {
    state().standing.clone()
}

/// How many times that has changed. Ask before [`standing`].
pub fn published() -> u64 {
    state().published
}

/// The note under the language in force, in the shell's language.
pub fn note() -> String {
    match standing() {
        None | Some(Standing::Spoken) => i18n::text("language-in-use").to_string(),
        Some(Standing::Changing) => i18n::text("language-changing").to_string(),
        Some(Standing::NoService) => i18n::text("language-in-use-no-service").to_string(),
        Some(Standing::Refused {
            why,
            session_follows: true,
        }) => crate::message!("language-in-use-not-system", "why" => why),
        Some(Standing::Refused {
            why,
            session_follows: false,
        }) => crate::message!("language-shell-only", "why" => why),
        // What was *wrong* — which variable in which file named another
        // language — is written to the journal and stays there. A row is read
        // by somebody who wants their console in Polish, not by somebody
        // debugging an environment, so both of these say only the one thing
        // that person can do about it. See [`repairs_in`].
        Some(Standing::Changed) => i18n::text("language-changed").to_string(),
        Some(Standing::NotEverywhere) => i18n::text("language-not-everywhere").to_string(),
    }
}

fn work(mut language: Language) {
    loop {
        let standing = carry_out(language);
        let mut state = state();
        state.standing = Some(standing);
        state.published += 1;
        match state.next.take() {
            Some(again) => language = again,
            None => {
                state.busy = false;
                return;
            }
        }
    }
}

/// One choice, all the way: the system, then the session.
///
/// Two roads to the system, and which one is taken is decided before anything
/// is asked of anybody. Where nothing on this machine names another language,
/// the locale service is asked from here and that is the whole job — one
/// password question, localed's own. Where something does — the `LC_ALL` in
/// `/etc/environment` that would otherwise keep the login screen in the old
/// language whatever localed wrote — the job is handed to [`apply_as_root`]
/// through polkit, which sets the system locale *and* changes those files,
/// still for one password question, because a root process does not have to
/// ask localed's permission to talk to localed.
///
/// The session is told whatever the system said, as long as glibc can load
/// the locale: the shell is the launcher, and needs nobody's leave to say what
/// its own children start in. Not when the locale is missing — and the system
/// having taken it is the proof that it is there, generated a moment ago if
/// need be.
fn carry_out(language: Language) -> Standing {
    let locale = locale_for(language);
    // The session's own environment files first. They are this account's, so
    // there is nothing to ask anybody, and one of them names a language as
    // firmly as anything in `/etc` does.
    for repair in repairs_in(&session_files(), locale) {
        match write_repair(&repair) {
            Ok(()) => tracing::info!(
                file = %repair.file.display(),
                variables = ?repair.variables,
                "the session's environment follows the language"
            ),
            Err(err) => tracing::warn!(
                file = %repair.file.display(),
                ?err,
                "the session's environment could not be changed"
            ),
        }
    }
    let machine = repairs_in(&machine_files(), locale);
    let (standing, follows) = match machine.is_empty() {
        true => how_it_went(set_system_locale(locale), locale),
        false => the_privileged_road(locale, &machine),
    };
    if follows {
        let written = export(locale, language.gettext_name());
        tell_the_session_bus(&written);
        tracing::info!(locale, ?written, "the session speaks the shell's language");
    }
    standing
}

/// What the locale service's answer means for the row, and whether the session
/// is told regardless.
fn how_it_went(answered: Result<(), Answer>, locale: &str) -> (Standing, bool) {
    match answered {
        Ok(()) => (Standing::Changed, true),
        Err(Answer::NotInstalled(why)) => (
            Standing::Refused {
                why,
                session_follows: false,
            },
            false,
        ),
        Err(_) if !installed(locale) => (
            Standing::Refused {
                why: crate::message!("language-locale-not-installed", "locale" => locale),
                session_follows: false,
            },
            false,
        ),
        Err(Answer::NoService) => (Standing::NoService, true),
        Err(Answer::Refused(why)) => (
            Standing::Refused {
                why,
                session_follows: true,
            },
            true,
        ),
    }
}

/// The road for a machine whose environment files name another language: one
/// short-lived root process, started by polkit, that does both halves.
///
/// A refusal here is not a refusal of the language — the shell's own, and
/// every program it opens from now on, still changes — so the session is told
/// either way. What is lost is the rest of the machine, which is what
/// [`Standing::NotEverywhere`] says and the row asks them to try again for.
fn the_privileged_road(locale: &str, repairs: &[Repair]) -> (Standing, bool) {
    for repair in repairs {
        tracing::info!(
            file = %repair.file.display(),
            variables = ?repair.variables,
            locale,
            "asking to change what the machine's environment says about the language"
        );
    }
    match ask_root_to_apply(locale) {
        Root::Done => (Standing::Changed, true),
        Root::NoService => (Standing::NoService, true),
        Root::NotAuthorized => {
            tracing::info!(locale, "nobody authorized the change to the machine");
            (Standing::NotEverywhere, true)
        }
        Root::Failed(why) => {
            tracing::warn!(
                locale,
                why,
                "the privileged half of the language change failed"
            );
            (Standing::NotEverywhere, true)
        }
        // Nothing ran, so nothing was asked of anybody and the plain road is
        // still worth taking: the locale service at least keeps the language
        // for the next start, even on a machine whose files go on outranking
        // it. A machine with no `pkexec` at all is the way here.
        Root::Unavailable(why) => {
            tracing::warn!(locale, why, "the privileged half could not be started");
            match how_it_went(set_system_locale(locale), locale) {
                (Standing::Changed, follows) => (Standing::NotEverywhere, follows),
                other => other,
            }
        }
    }
}

/// What localed said, when it did not say yes.
#[derive(Debug)]
enum Answer {
    /// No system bus, or nobody on it by that name.
    NoService,
    /// The one refusal that is about the locale rather than the person.
    NotInstalled(String),
    /// Everything else, in the daemon's words.
    Refused(String),
}

/// `SetLocale(["LANG=…"], interactive)` on the system bus.
///
/// Interactive both ways it can be said — the argument localed reads and the
/// message flag polkit reads — so that a password question reaches this
/// session's agent instead of being refused in seven milliseconds; see
/// [`crate::users`], where that flag's story is told. A locale the file
/// already names is answered with success and no question at all.
fn set_system_locale(locale: &str) -> Result<(), Answer> {
    let bus = zbus::blocking::Connection::system().map_err(|err| {
        tracing::debug!(?err, "no system bus; the language stays this session's");
        Answer::NoService
    })?;
    let proxy = zbus::blocking::Proxy::new(&bus, LOCALED, LOCALED_PATH, LOCALED_IFACE)
        .map_err(|err| Answer::Refused(why(&err)))?;
    let asked = crate::polkit::asked_so_far();
    let assignments = vec![format!("LANG={locale}")];
    let answered = proxy.call_with_flags::<_, _, ()>(
        "SetLocale",
        zbus::proxy::MethodFlags::AllowInteractiveAuth.into(),
        &(assignments, true),
    );
    match answered {
        Ok(_) => Ok(()),
        Err(err) => {
            tracing::warn!(locale, ?err, "the locale service refused");
            Err(match &err {
                zbus::Error::MethodError(name, detail, _) => {
                    let name = name.as_str();
                    let detail = detail.as_deref().unwrap_or_default();
                    if name.ends_with(".ServiceUnknown") || name.ends_with(".NameHasNoOwner") {
                        Answer::NoService
                    } else if detail.contains("not installed") {
                        Answer::NotInstalled(why(&err))
                    } else {
                        let mut why = why(&err);
                        if wanted_a_password(name) && crate::polkit::asked_so_far() == asked {
                            why.push(' ');
                            why.push_str(i18n::text(match crate::polkit::holds_the_session() {
                                true => "polkit-holds-panel-never-asked",
                                false => "polkit-not-this-sessions-agent",
                            }));
                        }
                        Answer::Refused(why)
                    }
                }
                _ => Answer::Refused(why(&err)),
            })
        }
    }
}

/// Whether the refusal was about proving who you are, by its name: what
/// localed says when polkit said no, and what the bus says when it did not
/// get to ask.
fn wanted_a_password(name: &str) -> bool {
    matches!(
        name.rsplit('.').next(),
        Some(
            "AccessDenied"
                | "PermissionDenied"
                | "NotAuthorized"
                | "InteractiveAuthorizationRequired"
        )
    )
}

/// The daemon's own words, fit to put on a row — the same shape
/// [`crate::users`] gives an account refusal, and for the same reason: the
/// message is often the exact sentence somebody needs, and the error's name
/// is not something to show anybody.
fn why(err: &zbus::Error) -> String {
    if let zbus::Error::MethodError(name, detail, _) = err {
        if let Some(detail) = detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            return match detail.ends_with(['.', '!', '?']) {
                true => detail.to_string(),
                false => format!("{detail}."),
            };
        }
        let tail = name.as_str().rsplit('.').next().unwrap_or(name.as_str());
        return crate::message!("locale-service-answered", "answer" => tail);
    }
    format!("{err}")
}

/// The locale `language` is given.
///
/// The one the session started in, where that already speaks the language:
/// English on a machine set up as `en_GB.UTF-8` stays British rather than
/// becoming American, because the choice was a language and not a country.
/// Otherwise the installer's first offer for it. `C` and `POSIX` are read as
/// English by the catalogs but are not a language anybody chose, so English
/// picked on such a machine gets a real one.
pub fn locale_for(language: Language) -> &'static str {
    locale_for_with(language, STARTED_IN.as_deref())
}

fn locale_for_with(language: Language, started_in: Option<&str>) -> &'static str {
    if let Some(started) = started_in {
        let bare = started.split('.').next().unwrap_or(started);
        if !matches!(bare, "C" | "POSIX") && i18n::spoken_by(started) == language {
            // Leaked once per distinct value, of which a session has one.
            return Box::leak(started.to_string().into_boxed_str());
        }
    }
    DEFAULT_LOCALE
        .iter()
        .find(|(candidate, _)| *candidate == language)
        .map(|(_, locale)| *locale)
        .unwrap_or("en_US.UTF-8")
}

/// Whether the session, as it stands now, speaks `language`.
pub fn session_speaks(language: Language) -> bool {
    i18n::spoken_by(&i18n::session_locale()) == language
}

/// Whether glibc can load `locale` on this machine — the test every program
/// the shell opens would make on its first line.
pub fn installed(locale: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(locale) else {
        return false;
    };
    // SAFETY: `newlocale` reads the locale archive and allocates an object
    // this frees at once; nothing is shared, nothing is set for the process.
    unsafe {
        let handle = libc::newlocale(libc::LC_ALL_MASK, name.as_ptr(), std::ptr::null_mut());
        if handle.is_null() {
            return false;
        }
        libc::freelocale(handle);
    }
    true
}

/// Write the locale into this process's environment, and say what was
/// written.
///
/// `LANG` always. Each of [`OVERRIDES`] only where the session already had
/// it: present, it outranks `LANG` and would keep the old language; absent,
/// `LANG` decides and adding it would grow the environment under other
/// threads' feet (see [`prime`]). `LANGUAGE` is gettext's list of languages
/// and takes the language, not the locale.
fn export(locale: &str, language: &str) -> Vec<(String, String)> {
    let written = export_with(locale, language, |name| std::env::var_os(name).is_some());
    for (name, value) in &written {
        // SAFETY: every name here is one the environment already holds —
        // `LANG` since [`prime`], the rest by the `present` test — so glibc
        // swaps a pointer and frees nothing; see [`prime`].
        unsafe { std::env::set_var(name, value) };
    }
    written
}

fn export_with(
    locale: &str,
    language: &str,
    present: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let mut written = vec![("LANG".to_string(), locale.to_string())];
    for name in OVERRIDES {
        if present(name) {
            let value = match name {
                "LANGUAGE" => language,
                _ => locale,
            };
            written.push((name.to_string(), value.to_string()));
        }
    }
    written
}

/// Tell the session bus and the systemd user manager, so a program either of
/// them starts on somebody's behalf follows too. Best effort: a session
/// without one or the other is not a fault, and is said at debug.
fn tell_the_session_bus(written: &[(String, String)]) {
    let bus = match zbus::blocking::Connection::session() {
        Ok(bus) => bus,
        Err(err) => {
            tracing::debug!(?err, "no session bus to tell about the language");
            return;
        }
    };
    let map: HashMap<&str, &str> = written
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    if let Err(err) = bus.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "UpdateActivationEnvironment",
        &(map,),
    ) {
        tracing::debug!(?err, "the session bus did not take the language");
    }
    let assignments: Vec<String> = written
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    if let Err(err) = bus.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "SetEnvironment",
        &(assignments,),
    ) {
        tracing::debug!(?err, "the user manager did not take the language");
    }
}

/// One environment file that names another language, and what it should say
/// instead: the whole new text, and the variables in it that changed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Repair {
    file: std::path::PathBuf,
    text: String,
    variables: Vec<String>,
}

/// The machine's environment files, `/etc/environment` first.
fn machine_files() -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = ENVIRONMENT_FILES
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    files.extend(conf_files(std::path::Path::new(ENVIRONMENT_DIRECTORY)));
    files
}

/// This account's, which the shell may write itself.
fn session_files() -> Vec<std::path::PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        });
    match config {
        Some(config) => conf_files(&config.join(SESSION_ENVIRONMENT_DIRECTORY)),
        None => Vec::new(),
    }
}

/// `*.conf` in a directory, in the order systemd reads them.
fn conf_files(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(reading) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: Vec<std::path::PathBuf> = reading
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|kind| kind == "conf"))
        .collect();
    found.sort();
    found
}

/// What on this machine names another language than `locale`, and what those
/// files should say instead.
///
/// `SetLocale` writes `LANG` into `/etc/locale.conf`, and that file is read
/// once, by the service manager, at boot. Every login session then has PAM
/// read `/etc/environment` over the top of it, and glibc reads `LC_ALL`,
/// `LC_MESSAGES` and `LANGUAGE` before it looks at `LANG` at all. A machine
/// whose `/etc/environment` says `LC_ALL=en_GB.UTF-8` therefore goes on
/// speaking English to everything this shell did not start — the login screen,
/// a service, a session on another seat — however carefully the system locale
/// was set.
///
/// That is what this finds, and [`apply_as_root`] is what puts it right. A
/// file is listed only when something in it names a *different language*: a
/// country somebody chose deliberately — `en_GB` where the shell would have
/// picked `en_US` — is theirs to keep, and so is everything in the file that
/// is not one of [`DECIDE_THE_LANGUAGE`].
fn repairs_in(files: &[std::path::PathBuf], locale: &str) -> Vec<Repair> {
    let mut repairs = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        if let Some((text, variables)) = repaired(&text, locale) {
            repairs.push(Repair {
                file: file.clone(),
                text,
                variables,
            });
        }
    }
    repairs
}

/// What one file should say instead, and which variables changed. `None`
/// where nothing in it names another language, which is the answer for nearly
/// every file on nearly every machine.
fn repaired(text: &str, locale: &str) -> Option<(String, Vec<String>)> {
    let mut out = String::with_capacity(text.len());
    let mut variables = Vec::new();
    // `split_inclusive` keeps each line's own ending, so a file that does not
    // end in a newline does not gain one and nothing else moves by a byte.
    for chunk in text.split_inclusive('\n') {
        let (line, ending) = match chunk.strip_suffix('\n') {
            Some(line) => (line, "\n"),
            None => (chunk, ""),
        };
        match repair_line(line, locale) {
            Some((variable, replacement)) => {
                variables.push(variable);
                out.push_str(&replacement);
            }
            None => out.push_str(line),
        }
        out.push_str(ending);
    }
    (!variables.is_empty()).then_some((out, variables))
}

/// One line, and what it should say instead.
///
/// `None` unless the line assigns one of [`DECIDE_THE_LANGUAGE`] a value that
/// names a different language: a comment, a blank line, a variable that is
/// none of the shell's business and a variable that already names the right
/// language are all left exactly as they are. Where it does answer, the
/// indent, the `export`, the name and the quotes come back unchanged and only
/// the value is different — this is rewriting one setting in a file it does
/// not own.
fn repair_line(line: &str, locale: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let indent = &line[..line.len() - trimmed.len()];
    let (keyword, rest) = match trimmed.strip_prefix("export ") {
        Some(rest) => ("export ", rest.trim_start()),
        None => ("", trimmed),
    };
    let (name, value) = rest.split_once('=')?;
    let name = name.trim();
    if !DECIDE_THE_LANGUAGE.contains(&name) {
        return None;
    }
    let value = value.trim();
    let quote = ['"', '\'']
        .into_iter()
        .find(|mark| value.len() >= 2 && value.starts_with(*mark) && value.ends_with(*mark));
    let bare = match quote {
        Some(_) => &value[1..value.len() - 1],
        None => value,
    };
    // `LANGUAGE` is gettext's list of languages, best first, and the first of
    // them is the one that decides.
    let says = bare.split(':').next().unwrap_or_default();
    let wanted = i18n::spoken_by(locale);
    if i18n::spoken_by(says) == wanted {
        return None;
    }
    let becomes = match name {
        "LANGUAGE" => wanted.gettext_name(),
        _ => locale,
    };
    let becomes = match quote {
        Some(mark) => format!("{mark}{becomes}{mark}"),
        None => becomes.to_string(),
    };
    Some((
        name.to_string(),
        format!("{indent}{keyword}{name}={becomes}"),
    ))
}

/// Write one file back, whole, in one step.
///
/// Through a temporary file beside it and a rename, so that a machine losing
/// power in the middle of this has either the old `/etc/environment` or the
/// new one and never half of either — a truncated one is a machine that comes
/// up without a `PATH`. The mode is carried over for the same reason: this is
/// somebody else's file, put back as it was found bar one value.
fn write_repair(repair: &Repair) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let file = &repair.file;
    let name = file
        .file_name()
        .ok_or_else(|| std::io::Error::other("an environment file with no name"))?;
    let mode = std::fs::metadata(file)
        .map(|found| found.permissions().mode() & 0o7777)
        .unwrap_or(0o644);
    let mut beside = std::ffi::OsString::from(".");
    beside.push(name);
    beside.push(format!(".lxb-{}", std::process::id()));
    let temporary = file.with_file_name(beside);
    let written = (|| {
        std::fs::write(&temporary, &repair.text)?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&temporary, file)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// What came back from the privileged half.
enum Root {
    /// The system locale was written and the machine's files agree with it.
    Done,
    /// The files agree with it, and there is no locale service on this machine
    /// to keep it for the next start.
    NoService,
    /// Nobody proved they were allowed to: the question was dismissed, the
    /// password was wrong, or the machine's policy said no. Nothing was
    /// changed, and nothing is asked a second time — somebody who has just
    /// said no is not owed another panel.
    NotAuthorized,
    /// It ran and failed, in its own words — for the journal, never for a row.
    Failed(String),
    /// Nothing ran and nobody was asked anything, so there is still a road
    /// worth taking.
    Unavailable(String),
}

/// The exit status [`apply_as_root`] uses for "done, but this machine has no
/// locale service". Everything else it has to say is an ordinary failure with
/// a line on stderr, which the shell puts in the journal.
const NO_SERVICE_STATUS: i32 = 3;

/// Ask polkit to run this same program as root, for one language.
///
/// `--disable-internal-agent` because the question belongs to this session's
/// own password panel — [`crate::polkit`] — and not to a text prompt on a
/// terminal nobody is looking at. The whole command line is this binary, one
/// flag and one locale name, and the flag is what the installed policy binds
/// the action to, so what polkit authorizes is this job and nothing else.
fn ask_root_to_apply(locale: &str) -> Root {
    let Some(pkexec) = pkexec() else {
        return Root::Unavailable("no pkexec on this machine".into());
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return Root::Unavailable(format!("{err}")),
    };
    let answered = std::process::Command::new(pkexec)
        .arg("--disable-internal-agent")
        .arg(&exe)
        .arg(APPLY_FLAG)
        .arg(locale)
        .stdin(std::process::Stdio::null())
        .output();
    let answered = match answered {
        Ok(answered) => answered,
        Err(err) => return Root::Unavailable(format!("{err}")),
    };
    let said = String::from_utf8_lossy(&answered.stderr).trim().to_string();
    match answered.status.code() {
        Some(0) => Root::Done,
        Some(NO_SERVICE_STATUS) => Root::NoService,
        // pkexec's own two, and both of them mean nobody was authorized: 126
        // is a panel somebody dismissed, and 127 is every other way the
        // authorization did not happen — a wrong password, a policy that says
        // no, a program it would not run. Neither is a reason to put a second
        // panel up, which is what taking the plain road from here would do.
        Some(126 | 127) => {
            tracing::debug!(said, "pkexec did not authorize the language change");
            Root::NotAuthorized
        }
        _ => Root::Failed(said),
    }
}

/// Where polkit's `pkexec` is, the wrapped one first: on NixOS the one on
/// `PATH` is not the setuid copy.
fn pkexec() -> Option<std::path::PathBuf> {
    let wrapped = std::path::Path::new("/run/wrappers/bin/pkexec");
    if wrapped.is_file() {
        return Some(wrapped.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join("pkexec"))
        .find(|candidate| candidate.is_file())
}

/// The privileged half of a language change: the system locale, and every
/// machine file that names another language.
///
/// Started by polkit, from [`ask_root_to_apply`], and by nothing else. It
/// takes one argument and answers to nothing in its environment, because
/// polkit hands it a clean one and the shell that asked is an ordinary
/// account's program that must not be able to steer it.
///
/// What bounds it is not this process's word for who started it — a caller
/// could lie about that — but what it is able to do at all. The locale name is
/// checked to be a locale name and to be one glibc can actually load, and the
/// only writing it can do is to change the value of a variable an environment
/// file already has, to that locale. It cannot add a line, remove one, or
/// write any variable other than the four that decide a language, so the worst
/// a caller can get out of it is the thing the action is for: the machine's
/// language changed.
pub fn apply_as_root(locale: &str) -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("this is polkit's half of a language change and only runs as root");
    }
    anyhow::ensure!(
        is_a_locale_name(locale),
        "{locale:?} is not the name of a locale"
    );
    anyhow::ensure!(
        installed(locale),
        "the {locale} locale is not installed on this machine"
    );
    let no_service = match set_system_locale(locale) {
        Ok(()) => false,
        Err(Answer::NoService) => true,
        Err(Answer::NotInstalled(why) | Answer::Refused(why)) => anyhow::bail!("{why}"),
    };
    for repair in repairs_in(&machine_files(), locale) {
        write_repair(&repair).map_err(|err| anyhow::anyhow!("{}: {err}", repair.file.display()))?;
        tracing::info!(
            file = %repair.file.display(),
            variables = ?repair.variables,
            locale,
            "the machine's environment follows the language"
        );
    }
    if no_service {
        std::process::exit(NO_SERVICE_STATUS);
    }
    Ok(())
}

/// Whether a string is shaped like a locale name, before anything is done
/// with it. POSIX allows `language[_TERRITORY][.codeset][@modifier]`, and
/// nothing in that has a newline, a space, a quote or a `$` in it — which is
/// what keeps one out of a file where a line is a variable.
fn is_a_locale_name(locale: &str) -> bool {
    !locale.is_empty()
        && locale.len() <= 64
        && locale
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_keeps_the_locale_the_session_started_in_when_it_already_speaks_it() {
        assert_eq!(
            locale_for_with(Language::British, Some("en_GB.UTF-8")),
            "en_GB.UTF-8"
        );
        assert_eq!(
            locale_for_with(Language::Polish, Some("en_GB.UTF-8")),
            "pl_PL.UTF-8"
        );
        assert_eq!(
            locale_for_with(Language::Polish, Some("pl_PL.utf8")),
            "pl_PL.utf8"
        );
        assert_eq!(
            locale_for_with(Language::British, Some("pl_PL.UTF-8")),
            "en_GB.UTF-8"
        );
        // C is read as English but is nobody's language.
        assert_eq!(
            locale_for_with(Language::British, Some("C.UTF-8")),
            "en_GB.UTF-8"
        );
        assert_eq!(
            locale_for_with(Language::British, Some("POSIX")),
            "en_GB.UTF-8"
        );
        assert_eq!(locale_for_with(Language::British, None), "en_GB.UTF-8");

        // The two Englishes are two locales, and a session already in one of
        // them keeps it rather than being moved to the other country's.
        assert_eq!(
            locale_for_with(Language::American, Some("en_US.UTF-8")),
            "en_US.UTF-8"
        );
        assert_eq!(
            locale_for_with(Language::American, Some("en_GB.UTF-8")),
            "en_US.UTF-8",
            "an American shell in a British session is given the American one"
        );
        assert_eq!(
            locale_for_with(Language::British, Some("en_US.UTF-8")),
            "en_GB.UTF-8",
            "and the other way round"
        );
        assert_eq!(
            locale_for_with(Language::British, Some("en_AU.UTF-8")),
            "en_AU.UTF-8",
            "every other English is read as British, so its own is kept"
        );
        assert_eq!(locale_for_with(Language::American, None), "en_US.UTF-8");

        // The two languages whose catalog names a region are read for the
        // whole language, so a session in the other variant keeps its own
        // locale — European Portuguese, traditional Chinese — and only a
        // session in neither is given the one the catalog was written in.
        assert_eq!(
            locale_for_with(Language::Portuguese, Some("pt_PT.UTF-8")),
            "pt_PT.UTF-8"
        );
        assert_eq!(
            locale_for_with(Language::Chinese, Some("zh_TW.UTF-8")),
            "zh_TW.UTF-8"
        );
        assert_eq!(
            locale_for_with(Language::Chinese, Some("de_DE.UTF-8")),
            "zh_CN.UTF-8"
        );
        for language in Language::CHOICES {
            let given = locale_for_with(language, None);
            assert_eq!(
                i18n::spoken_by(given),
                language,
                "{language:?} is given {given}, which is read as another language"
            );
        }
    }

    #[test]
    fn only_variables_the_session_already_had_are_rewritten() {
        let none = export_with("pl_PL.UTF-8", "pl", |_| false);
        assert_eq!(none, vec![("LANG".to_string(), "pl_PL.UTF-8".to_string())]);

        let all = export_with("pl_PL.UTF-8", "pl", |_| true);
        assert_eq!(
            all,
            vec![
                ("LANG".to_string(), "pl_PL.UTF-8".to_string()),
                ("LC_ALL".to_string(), "pl_PL.UTF-8".to_string()),
                ("LC_MESSAGES".to_string(), "pl_PL.UTF-8".to_string()),
                ("LANGUAGE".to_string(), "pl".to_string()),
            ]
        );

        let some = export_with("en_GB.UTF-8", "en", |name| name == "LANGUAGE");
        assert_eq!(
            some,
            vec![
                ("LANG".to_string(), "en_GB.UTF-8".to_string()),
                ("LANGUAGE".to_string(), "en".to_string()),
            ]
        );
    }

    #[test]
    fn the_c_locale_is_always_installed_and_a_made_up_one_never_is() {
        assert!(installed("C"));
        assert!(!installed("xx_YY.NOWHERE"));
        assert!(!installed("with\0nul"));
    }

    #[test]
    fn a_refusal_is_told_apart_by_its_name() {
        assert!(wanted_a_password("org.freedesktop.DBus.Error.AccessDenied"));
        assert!(wanted_a_password(
            "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired"
        ));
        assert!(!wanted_a_password("org.freedesktop.DBus.Error.InvalidArgs"));
    }

    /// Every shape pam_env allows, and what a language change does to each:
    /// the four that decide a language are given the new one, and every other
    /// byte in the file comes back as it went in.
    #[test]
    fn only_the_variables_that_decide_a_language_are_rewritten() {
        let before = "# the machine's environment\n\
                      \n\
                      LANG=en_GB.UTF-8\n\
                      export LC_ALL=\"en_GB.UTF-8\"\n\
                      LC_MESSAGES='en_GB.UTF-8'\n\
                      LANGUAGE=en:en_GB\n\
                      LC_TIME=en_GB.UTF-8\n\
                        PATH=/usr/bin  \n\
                      not an assignment\n";
        let (after, changed) = repaired(before, "pl_PL.UTF-8").expect("four to change");
        assert_eq!(changed, ["LANG", "LC_ALL", "LC_MESSAGES", "LANGUAGE"]);
        assert_eq!(
            after,
            "# the machine's environment\n\
             \n\
             LANG=pl_PL.UTF-8\n\
             export LC_ALL=\"pl_PL.UTF-8\"\n\
             LC_MESSAGES='pl_PL.UTF-8'\n\
             LANGUAGE=pl\n\
             LC_TIME=en_GB.UTF-8\n\
               PATH=/usr/bin  \n\
             not an assignment\n"
        );
        // And nothing at all to do on the same file once it has been done, or
        // on one that never named another language.
        assert_eq!(repaired(&after, "pl_PL.UTF-8"), None);
        assert_eq!(repaired("PATH=/usr/bin\n", "pl_PL.UTF-8"), None);
    }

    /// A country is not a language — except where the shell has a catalog for
    /// the country, which since the two Englishes were told apart is exactly
    /// `en_US`.
    ///
    /// So a machine set up as Australian and a shell set to English (UK) agree
    /// and the file is not touched, because every English but the American one
    /// is read out of the British catalog; and a machine set up as British and
    /// a shell set to English (US) do *not* agree, because those two really
    /// are two languages here and the whole point of the row is the date.
    #[test]
    fn a_country_somebody_chose_is_left_alone() {
        assert_eq!(repaired("LANG=en_AU.UTF-8\n", "en_GB.UTF-8"), None);
        assert_eq!(repaired("LANGUAGE=pl:en\n", "pl_PL.UTF-8"), None);
        assert!(repaired("LANG=en_GB.UTF-8\n", "pl_PL.UTF-8").is_some());
        assert_eq!(
            repaired("LANG=en_GB.UTF-8\n", "en_US.UTF-8"),
            Some(("LANG=en_US.UTF-8\n".to_string(), vec!["LANG".to_string()])),
            "the two Englishes are two languages"
        );
        // And `LANGUAGE` takes the locale name gettext splits, not the tag
        // `shell.toml` is written with.
        assert_eq!(
            repaired("LANGUAGE=pl\n", "en_US.UTF-8"),
            Some(("LANGUAGE=en_US\n".to_string(), vec!["LANGUAGE".to_string()]))
        );
    }

    /// A file that does not end in a newline does not gain one.
    #[test]
    fn a_file_is_written_back_byte_for_byte_but_for_the_value() {
        let (after, _) = repaired("LC_ALL=en_US.UTF-8", "pl_PL.UTF-8").expect("one to change");
        assert_eq!(after, "LC_ALL=pl_PL.UTF-8");
    }

    /// The flag the installed policy binds its action to is the flag this
    /// program answers to. They are the same word in three places — the
    /// constant, the command line and the policy file — and the third is read
    /// back against the first by `packaging/check.sh`.
    #[test]
    fn the_privileged_flag_is_the_one_the_command_line_takes() {
        use clap::Parser;
        let cli = crate::Cli::parse_from(["lxb-desktop", APPLY_FLAG, "pl_PL.UTF-8"]);
        assert_eq!(cli.apply_language.as_deref(), Some("pl_PL.UTF-8"));
    }

    /// What polkit's half will accept as the one thing it is told.
    #[test]
    fn a_locale_name_is_checked_before_anything_is_written() {
        assert!(is_a_locale_name("pl_PL.UTF-8"));
        assert!(is_a_locale_name("en_GB.UTF-8@euro"));
        assert!(!is_a_locale_name(""));
        assert!(!is_a_locale_name("pl_PL.UTF-8\nPATH=/tmp/evil"));
        assert!(!is_a_locale_name("pl_PL.UTF-8 PATH=/tmp"));
        assert!(!is_a_locale_name("$(id)"));
        assert!(!is_a_locale_name(&"x".repeat(65)));
    }

    /// The rule the user asked for: a session that has only just started never
    /// asks for a restart, whatever the machine has pinned. Nothing was
    /// changed while anybody was looking, so there is nothing to apply.
    #[test]
    fn a_session_that_has_just_started_never_asks_for_a_restart() {
        i18n::set(Language::British);
        for chosen in std::iter::once(Language::System).chain(Language::CHOICES) {
            for speaks in [true, false] {
                for have_it in [true, false] {
                    let standing = standing_at_startup(chosen, speaks, have_it);
                    assert!(
                        !matches!(standing, Standing::Changed | Standing::NotEverywhere),
                        "{chosen:?} speaks={speaks} installed={have_it} asked for a restart"
                    );
                    // And the one thing startup does say: a language whose
                    // locale this machine has not got.
                    let missing = chosen != Language::System && !speaks && !have_it;
                    assert_eq!(
                        matches!(standing, Standing::Refused { .. }),
                        missing,
                        "{chosen:?} speaks={speaks} installed={have_it}"
                    );
                }
            }
        }
    }

    /// And what a press finds, over files on disk, with the writing back that
    /// follows it — which is what polkit's half does to `/etc/environment`.
    #[test]
    fn a_file_that_names_another_language_is_found_and_put_right() {
        let scratch = std::env::temp_dir().join(format!("lxb-env-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("a scratch directory");
        let file = scratch.join("environment");
        let files = [file.clone()];

        std::fs::write(
            &file,
            "# the machine\nLANG=en_GB.UTF-8\nLC_ALL=en_GB.UTF-8\n",
        )
        .expect("an environment file");
        // The mode `/etc/environment` has, which it has to come back with:
        // this is root's file, and one that came back group-writable would be
        // a worse thing to have done than the language it was in.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
            .expect("the file's own mode");
        let found = repairs_in(&files, "pl_PL.UTF-8");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].variables, ["LANG", "LC_ALL"]);
        write_repair(&found[0]).expect("the file to be written back");
        assert_eq!(
            std::fs::read_to_string(&file).expect("the file back"),
            "# the machine\nLANG=pl_PL.UTF-8\nLC_ALL=pl_PL.UTF-8\n"
        );
        assert_eq!(
            std::fs::metadata(&file)
                .expect("the file back")
                .permissions()
                .mode()
                & 0o7777,
            0o644
        );
        // Done once, there is nothing left to do and nothing left to ask for.
        assert!(repairs_in(&files, "pl_PL.UTF-8").is_empty());
        // And the temporary the rename went through is not left behind.
        let left: Vec<_> = std::fs::read_dir(&scratch)
            .expect("the scratch directory")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(left, ["environment"]);

        assert!(repairs_in(&[scratch.join("nothing")], "pl_PL.UTF-8").is_empty());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Every standing's note, in one test because they share one standing:
    /// two tests writing it would be two tests reading each other's.
    ///
    /// The two a press can end in are the reason the rest of this module
    /// exists. The locale service takes the language and writes `LANG`, and a
    /// machine whose `/etc/environment` says `LC_ALL` goes on speaking the old
    /// language to everything the shell did not start — the login screen among
    /// them. Changing that file is what the first note is promising has
    /// happened and what the second is asking for a second try at; neither of
    /// them names a variable, a file or a daemon, because the person reading
    /// the row came to put their console in their own language.
    #[test]
    fn the_note_under_the_row_says_where_the_choice_stands() {
        i18n::set(Language::British);
        {
            let mut state = state();
            state.standing = Some(Standing::Spoken);
        }
        assert_eq!(note(), "In use by the shell and the system");
        {
            let mut state = state();
            state.standing = Some(Standing::Changed);
        }
        assert_eq!(
            note(),
            "In use by the shell and the system. A restart may be needed to apply it everywhere."
        );
        {
            let mut state = state();
            state.standing = Some(Standing::NotEverywhere);
        }
        assert_eq!(
            note(),
            "In use by the shell and what it opens. Choose it again to apply it everywhere."
        );
        {
            let mut state = state();
            state.standing = Some(Standing::Changing);
        }
        assert_eq!(note(), "Changing the system language…");
        {
            let mut state = state();
            state.standing = Some(Standing::Refused {
                why: "Access denied.".into(),
                session_follows: true,
            });
        }
        assert_eq!(
            note(),
            "In use by the shell and what it opens; the system's own setting was not changed. Access denied."
        );
        {
            let mut state = state();
            state.standing = Some(Standing::Refused {
                why: "Specified locale is not installed: LANG=pl_PL.UTF-8.".into(),
                session_follows: false,
            });
        }
        assert_eq!(
            note(),
            "In use by the shell only. Specified locale is not installed: LANG=pl_PL.UTF-8."
        );
        {
            let mut state = state();
            state.standing = None;
        }
    }
}
