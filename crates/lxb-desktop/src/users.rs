//! Who this machine is for: the accounts on it, and what makes another one.
//!
//! ## Why this talks to a daemon rather than to the files
//!
//! An account is four files — `/etc/passwd`, `/etc/shadow`, `/etc/group`, and a
//! home directory that has to exist with the right owner on it — and every one
//! of them is root's. A shell cannot write them, and a shell that ran `useradd`
//! under `pkexec` would be shipping a policy file of its own saying that this
//! session may run one program as root with arguments it chose, which is a
//! larger thing to be granted than the job needs.
//!
//! What is already on a Linux machine that has accounts is `accounts-daemon`,
//! and it owns all four halves: it makes the user, adds them to the
//! administrators' group or takes them out of it, writes the hash into the
//! shadow file, and keeps the picture. Every method on it is behind a polkit
//! action, so what decides whether a press is allowed is the machine's own
//! policy — and the panel that collects the proof is this shell's own agent,
//! which is already registered. See [`crate::polkit`]: the two halves of this
//! feature were built years apart and meet without either knowing about the
//! other.
//!
//! The cost is stated plainly on the page, exactly as [`crate::network`] states
//! its own: a session with no `accounts-daemon` gets one row saying so rather
//! than an empty column.
//!
//! ## What it does not do
//!
//! It does not offer shells, home directory paths, groups beyond the
//! administrator's, expiry policies, automatic login or enterprise directories.
//! Those are not one press and a name — they are a form of a different order —
//! and a console shell that offered half of one would be worse than one that
//! says the rest is set up elsewhere.
//!
//! It does not hash the password on this thread either. That is
//! [`crate::crypt`], and it happens on the worker: five thousand digests is a
//! frame and a half, and the frame it would spoil is the one where the user has
//! just pressed the row.
//!
//! ## The form is a draft, and the draft lives here
//!
//! Every other page in the Settings tree is a list of answers, and a press on
//! one of them is the whole change. These two are not: a new account is a name,
//! a user name, a kind, a password and a picture, and none of them means
//! anything without the others. So the rows write into a [`Draft`] held here,
//! the tree is *drawn* from that draft, and one row at the foot of it hands the
//! whole thing over.
//!
//! It lives in a static rather than on the row for the reason nothing in this
//! tree lives on a row: the Settings column is rebuilt from live state several
//! times a minute — a network appears, a battery moves — and a half-typed user
//! name kept in the catalogue would be thrown away by the next rebuild.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::secret::Secret;

/// How often the accounts are read again while the page is on screen.
///
/// Slower than the network's two seconds, because what is being watched moves
/// far more slowly: an account appears when somebody on this page makes one, or
/// when a second session makes one. Five seconds is a change showing up before
/// the user has finished noticing it themselves, for one D-Bus call plus one
/// per account.
const REFRESH: Duration = Duration::from_secs(5);

const ACCOUNTS: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
const ACCOUNTS_IFACE: &str = "org.freedesktop.Accounts";
const USER_IFACE: &str = "org.freedesktop.Accounts.User";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const LOGIN: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";
const LOGIN_IFACE: &str = "org.freedesktop.login1.Manager";

/// `AccountType`: what `accounts-daemon` calls the two kinds of account.
const STANDARD: i32 = 0;
const ADMINISTRATOR: i32 = 1;

/// The longest a user name may be.
///
/// Not a rule this shell invented: it is what `useradd` on Linux will take, and
/// a name longer than it is refused by the tools rather than truncated.
const LONGEST_NAME: usize = 32;

/// The longest a person's own name may be, which is a bound on what one field
/// of `/etc/passwd` will hold rather than a policy about names.
const LONGEST_REAL_NAME: usize = 255;

// --- what the machine has --------------------------------------------------

/// One account on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub uid: u64,
    /// Where the account hangs on the bus. Carried because every change is made
    /// to that object, and looking it up again by name would be a second chance
    /// to act on the wrong account.
    pub path: String,
    /// The user name — what they log in as.
    pub name: String,
    /// The name they are called by, where they have set one. Empty for an
    /// account nobody has named, which is what [`Person::title`] answers.
    pub real: String,
    pub admin: bool,
    /// Their picture, if the file the account names is really there.
    ///
    /// Checked rather than trusted: `IconFile` goes on pointing at a file
    /// somebody deleted, and a row that asked the thumbnailer for it would ask
    /// once a frame for the rest of the session. See [`crate::thumbs`], which
    /// remembers a failure but should not have to.
    pub picture: Option<PathBuf>,
    /// When that picture was last written, in whole seconds.
    ///
    /// Carried because an avatar is a file whose *path never changes*:
    /// `accounts-daemon` copies whatever is chosen to
    /// `/var/lib/AccountsService/icons/<name>` and leaves it there. Everything
    /// downstream of this keys pictures by path — the atlas the shell draws
    /// from does, and it never asks twice for a path it already holds — so
    /// without a stamp somebody who changes their avatar goes on wearing the
    /// old one until the session ends. See `Shell::sync_users`, which is where
    /// a change to this number throws the old picture away.
    pub stamp: Option<u64>,
    pub home: PathBuf,
    /// Whether they are logged in somewhere on this machine right now — which
    /// is what decides whether their account can be removed.
    pub here: bool,
    /// Whether this is the account the session is running as.
    pub you: bool,
}

impl Person {
    /// What the row is called: their own name, or their user name where they
    /// have not got one.
    pub fn title(&self) -> &str {
        match self.real.trim().is_empty() {
            true => &self.name,
            false => self.real.trim(),
        }
    }

    /// The line under it: who they log in as, what they may do, and whether
    /// they are here.
    pub fn note(&self) -> String {
        let kind = match self.admin {
            true => crate::i18n::text("shell-administrator"),
            false => crate::i18n::text("shell-standard"),
        };
        let mut note = format!("{} — {kind}", self.name);
        // "You" rather than "Signed in" for the account this session is: they
        // are both true, and the one that decides what the page will let them
        // do is the second.
        if self.you {
            note.push_str(" — you");
        } else if self.here {
            note.push_str(" — signed in");
        }
        note
    }
}

/// Every account on this machine, as the worker last found them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Whether `accounts-daemon` answered at all. `false` is the row that says
    /// there is nothing here to configure with.
    pub daemon: bool,
    /// The human accounts, by user id — which is the order they were made in,
    /// and the only order that does not move under a cursor when somebody
    /// renames themselves.
    pub people: Vec<Person>,
    /// What went wrong with the last thing somebody asked for, if it did.
    ///
    /// Kept in the listing rather than raised as a panel, because the page is
    /// where the press was made and the page is still on screen: a refusal
    /// belongs on the row that asked for it. Cleared by the next ask.
    pub trouble: Option<Trouble>,
    /// Whether something asked for is still in flight — which on this page is
    /// nearly always polkit waiting for a password, and can take as long as
    /// somebody takes to type one.
    pub working: bool,
}

/// A refusal, in two parts.
///
/// Two parts because they come from two different places and only one of them
/// is this shell's. `what` is what the shell was trying to do, written here;
/// `why` is what the machine said about it, and the machine is the only thing
/// that knows. A row with only the first half is where this page started, and
/// it is worth nobody's morning: "That did not work" is the same sentence
/// whether polkit refused, `usermod` found the account busy, or there was no
/// daemon at the other end, and each of those wants a different thing done
/// about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trouble {
    /// What was being done, as the title of the row. No full stop: it is a
    /// heading, and every other heading on this page goes without one.
    pub what: String,
    /// Why it did not happen — the daemon's own words wherever there are any.
    pub why: String,
}

impl Trouble {
    fn new(what: &str, why: impl Into<String>) -> Self {
        Self {
            what: what.to_string(),
            why: why.into(),
        }
    }
}

impl Listing {
    pub fn person(&self, uid: u64) -> Option<&Person> {
        self.people.iter().find(|person| person.uid == uid)
    }

    /// How many of them may administer the machine.
    pub fn admins(&self) -> usize {
        self.people.iter().filter(|person| person.admin).count()
    }

    /// Whether this account is the only administrator left.
    ///
    /// What hides the Account type rows on their page, and what refuses to
    /// remove them. Both for one reason: a machine with no administrator on it
    /// is a machine nobody can install anything on, change the clock on, or
    /// make another account on — and it cannot be undone from this shell,
    /// because every one of those needs an administrator to agree to it.
    pub fn only_admin(&self, uid: u64) -> bool {
        self.person(uid).is_some_and(|person| person.admin) && self.admins() <= 1
    }
}

// --- the form --------------------------------------------------------------

/// Which account a form is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    /// The account that does not exist yet.
    New,
    /// One that does, by user id — not by object path, because the path is a
    /// string and this is compared on every frame.
    Existing(u64),
}

/// One value on a form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Username,
    Password,
    Confirm,
}

impl Field {
    pub fn title(self) -> &'static str {
        match self {
            Field::Name => crate::i18n::text("shell-name"),
            Field::Username => crate::i18n::text("shell-username"),
            Field::Password => crate::i18n::text("shell-password"),
            Field::Confirm => crate::i18n::text("shell-confirm-password"),
        }
    }

    /// What to type, for somebody looking at an empty field on a television.
    pub fn note(self) -> &'static str {
        match self {
            Field::Name => {
                crate::i18n::text("shell-what-this-person-is-called-as-it-appears-on-screen")
            }
            Field::Username => crate::i18n::text("users-username-rules"),
            Field::Password => crate::i18n::text("shell-what-they-type-to-log-in"),
            Field::Confirm => crate::i18n::text(
                "shell-the-same-password-again-so-a-mistyped-one-cannot-lock-them-out",
            ),
        }
    }

    /// Whether the field is drawn as a count of marks rather than as what was
    /// typed — see [`crate::dialog::Line::Secret`].
    pub fn secret(self) -> bool {
        matches!(self, Field::Password | Field::Confirm)
    }
}

/// Whether this form asks for the password a second time.
///
/// A new account only, and the reason is what a confirmation is *for*: it stops
/// a mistyped password locking somebody out of an account **nobody can get into
/// yet**. A new account's password is the only way in and has never been used,
/// so a typo in it is discovered by the person it was made for, at the login
/// screen, with no way back.
///
/// An account that already exists is not in that position. It is being changed
/// by somebody who has just proved to polkit that they may, the old password
/// went on working until this moment, and a typo is put right by typing it
/// again. Asking twice there is ceremony — and on a console keyboard, which is
/// a thumbstick and a grid of letters, ceremony is expensive.
///
/// One function rather than a rule written on both sides, because the two sides
/// must not disagree: a tree that dropped the row while the check still wanted
/// a match would refuse every save, with nothing on the page to say why.
pub fn asks_twice(whose: Whose) -> bool {
    matches!(whose, Whose::New)
}

/// The form as the tree draws it: everything but the passwords themselves.
///
/// A copy rather than a borrow, because the caller is building a column and the
/// lock must not be held while it does. The passwords are counts here and stay
/// counts — nothing outside this module ever sees one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub whose: Whose,
    pub real: String,
    pub name: String,
    pub admin: bool,
    pub picture: Option<PathBuf>,
    /// How many characters are in the two password fields.
    pub typed: usize,
    pub confirmed: usize,
    /// Whether the picture was chosen on this form rather than carried in from
    /// the account. What decides whether saving touches `IconFile` at all.
    pub chose_picture: bool,
}

/// The form being filled in, if one is open. See the module header for why it
/// lives here and not on the row.
static FORM: Mutex<Option<Draft>> = Mutex::new(None);

/// The last listing, for the checks a field makes against the machine.
///
/// The same shape [`crate::settings`] keeps its own copies in, and for the same
/// reason: [`fault`] is asked by a panel that has no worker to hand.
static KNOWN: Mutex<Option<Listing>> = Mutex::new(None);

struct Draft {
    whose: Whose,
    real: String,
    name: String,
    admin: bool,
    picture: Option<PathBuf>,
    chose_picture: bool,
    password: Secret,
    confirm: Secret,
}

/// Start a form: a new account, or the one this person already has.
///
/// A form for an existing account opens holding what that account *is*, so the
/// rows say the truth before anything is typed and a user who came to change
/// one thing leaves the rest alone by doing nothing.
pub fn open_form(whose: Whose, from: Option<&Person>) {
    let mut form = held(&FORM);
    // Already open on this account: leave it exactly as it is. This is asked
    // once a frame — see `Shell::sync_user_form` — and a version that rebuilt
    // the draft would wipe out whatever had been typed since the last one.
    if form.as_ref().is_some_and(|draft| draft.whose == whose) {
        return;
    }
    tracing::debug!(?whose, "opening an account form");
    *form = Some(Draft {
        whose,
        real: from.map(|person| person.real.clone()).unwrap_or_default(),
        name: from.map(|person| person.name.clone()).unwrap_or_default(),
        // A new account is Standard until somebody says otherwise. The
        // conservative half of the answer, and the one a machine that already
        // has an administrator wants for the second person on it.
        admin: from.is_some_and(|person| person.admin),
        picture: from.and_then(|person| person.picture.clone()),
        chose_picture: false,
        password: Secret::default(),
        confirm: Secret::default(),
    });
}

/// The form, for a column being built.
pub fn form() -> Option<Form> {
    held(&FORM).as_ref().map(|draft| Form {
        whose: draft.whose,
        real: draft.real.clone(),
        name: draft.name.clone(),
        admin: draft.admin,
        picture: draft.picture.clone(),
        typed: draft.password.typed(),
        confirmed: draft.confirm.typed(),
        chose_picture: draft.chose_picture,
    })
}

/// The form, if it is the one about this account.
pub fn form_for(whose: Whose) -> Option<Form> {
    form().filter(|form| form.whose == whose)
}

/// Throw the form away, passwords and all.
///
/// Called when the cursor walks out of it. The [`Secret`]s go here, which is
/// the whole reason this is a function and not a `None` written from outside:
/// dropping them is what overwrites them.
pub fn close_form() {
    if held(&FORM).take().is_some() {
        tracing::debug!("the account form was left");
    }
}

/// Put what was typed into one of the fields that is not a secret.
pub fn write(field: Field, text: &str) {
    let mut form = held(&FORM);
    let Some(draft) = form.as_mut() else {
        return;
    };
    match field {
        Field::Name => draft.real = text.trim().to_string(),
        Field::Username => draft.name = text.trim().to_string(),
        // The two that are. They never arrive here: a password that had been
        // through a `&str` would be a copy of it in an allocation nothing
        // overwrites, which is the one thing `Secret` exists to prevent.
        Field::Password | Field::Confirm => {
            tracing::error!(?field, "a password was nearly written down as text")
        }
    }
}

/// The same, for one that is.
pub fn write_secret(field: Field, secret: Secret) {
    let mut form = held(&FORM);
    let Some(draft) = form.as_mut() else {
        return;
    };
    match field {
        Field::Password => draft.password = secret,
        Field::Confirm => draft.confirm = secret,
        Field::Name | Field::Username => {
            tracing::error!(?field, "a plain value arrived as a secret")
        }
    }
}

/// Which kind of account the form is for.
pub fn set_admin(admin: bool) {
    if let Some(draft) = held(&FORM).as_mut() {
        draft.admin = admin;
    }
}

/// The picture somebody walked to and pressed.
pub fn set_picture(file: &Path) {
    if let Some(draft) = held(&FORM).as_mut() {
        draft.picture = Some(file.to_path_buf());
        draft.chose_picture = true;
        tracing::info!(file = %file.display(), "a picture for the account");
    }
}

/// Take the picture back off, so the row wears the single figure again.
pub fn drop_picture() {
    if let Some(draft) = held(&FORM).as_mut() {
        draft.picture = None;
        // Chosen, even though what was chosen is nothing: saving has to write
        // the empty `IconFile` that takes the old one away.
        draft.chose_picture = true;
    }
}

/// Remember the listing, so [`fault`] can ask the machine about a name.
///
/// `true` when it is different from the one already here, which is what tells
/// the shell to rebuild the column. The same shape as
/// [`crate::settings::note_network`], and for the same reason.
pub fn note(listing: Listing) -> bool {
    let mut known = held(&KNOWN);
    if known.as_ref() == Some(&listing) {
        return false;
    }
    *known = Some(listing);
    true
}

/// The listing as it was last noted.
pub fn listing() -> Listing {
    held(&KNOWN).clone().unwrap_or_default()
}

/// What is wrong with what was typed into one field, or nothing if it will do.
///
/// Asked while the panel is still on screen and can still say so — the same
/// division [`crate::network::fault`] is under, and for the same reason: a
/// field that accepted anything and failed a second later, with the panel gone
/// and the keyboard with it, is a field with no answer to "why did nothing
/// happen".
///
/// It takes `whose` because two of the answers depend on it. A user name is
/// taken or free *for this account* — somebody editing their own account and
/// leaving the name alone has not asked for a name that is taken — and a
/// password may be left empty on an account that already has one and may not on
/// one that does not exist yet.
pub fn fault(whose: Whose, field: Field, text: &str) -> Option<&'static str> {
    match field {
        Field::Name => fault_in_real_name(text),
        Field::Username => fault_in_name(whose, text).or_else(|| fault_in_rename(whose, text)),
        Field::Password => fault_in_password(whose, text),
        // Checked against the other field rather than on its own, which is the
        // whole of what a confirmation is.
        Field::Confirm => {
            let matches = held(&FORM)
                .as_ref()
                .and_then(|draft| draft.password.as_text(|typed| typed == text))
                .unwrap_or(false);
            match matches {
                true => None,
                false => Some(crate::i18n::text(
                    "shell-that-is-not-the-same-as-the-password-above",
                )),
            }
        }
    }
}

fn fault_in_real_name(text: &str) -> Option<&'static str> {
    let text = text.trim();
    // Allowed to be empty: an account with no name of its own is listed under
    // the name it logs in as, which is what a great many accounts on a great
    // many machines are.
    if text.chars().count() > LONGEST_REAL_NAME {
        return Some(crate::i18n::text("shell-that-name-is-too-long"));
    }
    // The one character that cannot be in it: `/etc/passwd` is a colon-
    // separated file, and a name with one in it would end the field early.
    if text.contains(':') || text.contains('\n') {
        return Some(crate::i18n::text(
            "shell-a-name-cannot-contain-a-colon-or-a-line-break",
        ));
    }
    None
}

fn fault_in_name(whose: Whose, text: &str) -> Option<&'static str> {
    fault_in_name_among(whose, text, taken)
}

/// The same, asked of a machine somebody else describes.
///
/// Split out so the rule can be tested without the box the tests run on. The
/// shape of a user name is this shell's to decide and is checked here; whether
/// one is *taken* is a fact about a machine, and a test that asserted which
/// names this one has would be asserting the developer's own account. See the
/// note on reading the machine from a test.
fn fault_in_name_among(
    whose: Whose,
    text: &str,
    taken: impl Fn(&str) -> Option<u64>,
) -> Option<&'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Some(crate::i18n::text(
            "shell-a-user-name-is-needed-it-is-what-they-log-in-as",
        ));
    }
    if text.chars().count() > LONGEST_NAME {
        return Some(crate::i18n::text(
            "shell-a-user-name-can-be-at-most-32-characters",
        ));
    }
    // The portable rule, which is what every tool on the machine will accept
    // and rather narrower than what some of them would. A name outside it is
    // refused here rather than by `useradd` three layers down, where the answer
    // would arrive as a failed press with no reason on it.
    let mut characters = text.chars();
    let first = characters.next()?;
    if !first.is_ascii_lowercase() && first != '_' {
        return Some(crate::i18n::text(
            "shell-a-user-name-has-to-start-with-a-lower-case-letter-or",
        ));
    }
    if !characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Some(crate::i18n::text(
            "shell-only-lower-case-letters-digits-and-are-allowed",
        ));
    }
    // Taken, and by somebody else. Asked of the machine rather than of the
    // listing, because the listing holds only the people: `root`, `daemon` and
    // every service account on the box are names that cannot be had, and none
    // of them is on this page.
    if let Some(uid) = taken(text) {
        let yours = matches!(whose, Whose::Existing(existing) if existing == uid);
        if !yours {
            return Some(crate::i18n::text(
                "shell-somebody-on-this-machine-already-has-that-user-name",
            ));
        }
    }
    None
}

/// Whether this account can be renamed at all right now.
///
/// It cannot, while whoever it belongs to is signed in, and that is not a rule
/// this shell invented: renaming an account is `usermod -l`, and `usermod`
/// refuses outright — "user X is currently used by process N" — for any change
/// that moves what a running process is standing on. Its own manual says so.
/// There is no flag that overrides it and no order of operations that avoids
/// it; the session holding the name has to end first.
///
/// So it is said here, on the field, while the panel that asked is still up —
/// the same bargain as every other refusal on this page. Left to the daemon it
/// would arrive as a failed save with the reason on a different row, which is
/// exactly what it did arrive as: a user name that would not change and nothing
/// on screen saying why.
fn fault_in_rename(whose: Whose, text: &str) -> Option<&'static str> {
    let Whose::Existing(uid) = whose else {
        return None;
    };
    let listing = listing();
    let person = listing.person(uid)?;
    // Only a *change* is refused. Somebody who came to set a picture and never
    // touched this field has not asked for anything.
    if person.name == text.trim() {
        return None;
    }
    match person.you {
        true => Some(crate::i18n::text(
            "shell-you-cannot-be-renamed-while-you-are-signed-in",
        )),
        false => match person.here {
            true => Some(crate::i18n::text(
                "shell-they-are-signed-in-and-cannot-be-renamed-until-they-sign-out",
            )),
            false => None,
        },
    }
}

fn fault_in_password(whose: Whose, text: &str) -> Option<&'static str> {
    // An empty password on an existing account means "leave it alone", which is
    // what somebody who came to change the picture has typed. On a new account
    // there is nothing to leave alone.
    if text.is_empty() {
        return match whose {
            Whose::New => Some(crate::i18n::text("shell-a-new-account-needs-a-password")),
            Whose::Existing(_) => None,
        };
    }
    // There is no shortest password, and that is a decision rather than an
    // omission. A rule about length is a judgement about somebody else's
    // machine, and the one place it would really be felt is a console keyboard
    // — a thumbstick over a grid of letters — where a person who wants four
    // characters has thought harder about the cost of typing than any rule
    // here has. Whatever they choose is the password.
    //
    // The one bound left is not a policy at all but the width of the field:
    // what `Secret` will hold. A longer one is not refused by anything down the
    // line — it is simply not all there, and a password that is silently cut is
    // one nobody can log in with.
    if text.len() > 255 {
        return Some(crate::i18n::text("shell-that-password-is-too-long"));
    }
    None
}

/// Whether any account on this machine already has this user name, and whose.
///
/// `getpwnam` rather than a read of `/etc/passwd`, so that a machine whose
/// accounts come from somewhere else — a directory, `systemd-homed` — answers
/// for its own accounts too.
fn taken(name: &str) -> Option<u64> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: `name` is a valid C string that outlives the call, and the
    // pointer that comes back is either null or into libc's own storage, which
    // is only read here and never kept.
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        return None;
    }
    // SAFETY: not null, so it points at a `passwd` libc owns.
    Some(unsafe { (*entry).pw_uid }.into())
}

/// What still stands between the form and the account it describes.
///
/// Everything [`fault`] asks of one field at a time, asked again of the whole
/// form — because a field is only checked when somebody presses Set on its
/// panel, and a form can perfectly well be filled in by pressing nothing at
/// all. It is what the row at the foot of the page says, and what stops that
/// row from doing anything.
pub fn fault_in_form_for(whose: Whose) -> Option<&'static str> {
    let form = held(&FORM);
    let Some(draft) = form.as_ref().filter(|draft| draft.whose == whose) else {
        // No draft for this account, which is every frame before the cursor has
        // stepped into its column — the shell opens one when it does, and this
        // page is built long before that. What the row says then is what an
        // unstarted form would say: a new account is missing everything, and an
        // existing one is already an account and has nothing missing.
        return match whose {
            Whose::New => fault_in_name(whose, ""),
            Whose::Existing(_) => None,
        };
    };
    if let Some(fault) = fault_in_real_name(&draft.real) {
        return Some(fault);
    }
    if let Some(fault) = fault_in_name(draft.whose, &draft.name) {
        return Some(fault);
    }
    if let Some(fault) = fault_in_rename(draft.whose, &draft.name) {
        return Some(fault);
    }
    // The password, asked of the [`Secret`] itself: the text exists only inside
    // the closure, which is the whole of the care this can take.
    let fault = draft
        .password
        .as_text(|typed| fault_in_password(draft.whose, typed))
        .flatten();
    if let Some(fault) = fault {
        return Some(fault);
    }
    // And that the two agree, on the form that asks twice. An account that
    // already exists has no Confirm row at all — see [`asks_twice`] — so there
    // is nothing there to agree with, and a check that still wanted one would
    // refuse every change to an account whose password had just been set. That
    // is not hypothetical: it is the bug this rule was written to fix.
    if asks_twice(draft.whose) && (!draft.password.is_empty() || !draft.confirm.is_empty()) {
        let same = draft
            .password
            .as_text(|typed| draft.confirm.as_text(|again| typed == again))
            .flatten()
            .unwrap_or(false);
        if !same {
            return Some(crate::i18n::text(
                "shell-the-two-passwords-are-not-the-same",
            ));
        }
    }
    // A picture that has gone between being chosen and being saved. Worth
    // saying, because the row above still names it and the daemon's own refusal
    // would arrive as a failed save with the reason on a different row.
    if draft.chose_picture {
        if let Some(picture) = draft.picture.as_deref() {
            if !picture.is_file() {
                return Some(crate::i18n::text(
                    "shell-the-picture-that-was-chosen-is-no-longer-there",
                ));
            }
        }
    }
    None
}

fn held<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One test at a time through the draft and the listing.
///
/// [`FORM`] and [`KNOWN`] are statics because a session has exactly one form
/// open and one machine to describe — see the module header — but the test
/// binary runs its tests in threads of one process, so two that both open a
/// form are each pulling it out from under the other. It is not hypothetical:
/// it flaked once before this existed.
///
/// Held for the length of a test rather than around each call, because what is
/// being checked is what a *sequence* of presses leaves behind, and a lock taken
/// per call would let another test in between two of them. The draft is thrown
/// away on the way in, so every test starts from nothing whatever ran last.
///
/// Shared with [`crate::settings`]'s own tests, which build the page these
/// values are drawn into, so the two cannot race each other either.
#[cfg(test)]
pub fn one_form_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ORDER: Mutex<()> = Mutex::new(());
    let guard = ORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    close_form();
    guard
}

// --- the worker ------------------------------------------------------------

/// One thing somebody pressed, on its way to `accounts-daemon`.
enum Ask {
    /// Make the account this form describes.
    Create(Made),
    /// Change the account this form is about to what it now says.
    Save(Made),
    /// Take an account off the machine, with or without what is in its home
    /// directory.
    Remove { uid: u64, path: String, files: bool },
}

/// A form, taken out of the draft and on its way to the worker.
///
/// The password is a [`Secret`] the whole way: it is moved out of the draft
/// into this, moved into the worker's own frame, hashed inside
/// [`Secret::as_text`], and dropped — which overwrites it. What crosses the bus
/// is the hash.
struct Made {
    whose: Whose,
    /// The account's object path, for a form about one that exists.
    path: Option<String>,
    real: String,
    name: String,
    admin: bool,
    picture: Option<PathBuf>,
    chose_picture: bool,
    password: Secret,
    /// What the account was when the form opened, so that saving touches only
    /// what really changed. `None` for a new account, where everything is new.
    was: Option<Person>,
}

/// The accounts, and the worker that keeps them true.
pub struct Users {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    signal: Condvar,
}

#[derive(Default)]
struct State {
    listing: Listing,
    /// Bumped every time the worker puts a *different* listing here, so the
    /// shell can ask "has anything changed" once a frame without copying the
    /// whole thing. See [`crate::network::Net::published`].
    published: u64,
    /// Whether the pages that show it are on screen.
    watching: bool,
    asks: Vec<Ask>,
    dirty: bool,
    done: bool,
}

impl Users {
    /// Start looking.
    ///
    /// Nothing is read until somebody opens the page — see [`Users::watch`] —
    /// so this costs a thread and a sleeping condition variable on a session
    /// that never opens Settings.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(err) = std::thread::Builder::new()
            .name("lxb-users".to_string())
            .spawn(move || Worker::new(worker).run())
        {
            tracing::warn!(?err, "no worker thread; the Users page is off");
        }
        Self { shared }
    }

    /// The accounts, as the worker last found them.
    pub fn listing(&self) -> Listing {
        self.state().listing.clone()
    }

    /// How many times that has changed. Ask before [`Users::listing`].
    pub fn published(&self) -> u64 {
        self.state().published
    }

    /// Say whether the page that shows it is on screen.
    pub fn watch(&self, looking: bool) {
        let mut state = self.state();
        if state.watching == looking {
            return;
        }
        state.watching = looking;
        if looking {
            state.dirty = true;
            self.shared.signal.notify_one();
        }
    }

    /// Hand the open form over: make the account, or save the changes.
    ///
    /// The draft is taken out here rather than left for the worker to read,
    /// because the two live in different places on purpose — the draft is the
    /// shell's and the ask is the worker's, and a worker reaching into a form
    /// somebody is still typing into would carry out half of it.
    ///
    /// `false` when there is nothing to hand over, which is a press on a row
    /// that should not have been pressable.
    pub fn submit(&self) -> bool {
        let mut form = held(&FORM);
        let Some(draft) = form.as_mut() else {
            return false;
        };
        let listing = listing();
        let was = match draft.whose {
            Whose::New => None,
            Whose::Existing(uid) => listing.person(uid).cloned(),
        };
        let made = Made {
            whose: draft.whose,
            path: was.as_ref().map(|person| person.path.clone()),
            real: draft.real.clone(),
            name: draft.name.clone(),
            admin: draft.admin,
            picture: draft.picture.clone(),
            chose_picture: draft.chose_picture,
            // Moved rather than copied: what is left behind is an empty
            // `Secret`, and the one that carries the password is dropped by the
            // worker the moment it has been hashed.
            password: std::mem::take(&mut draft.password),
            was,
        };
        // A form about an account that has gone between opening and pressing.
        if matches!(made.whose, Whose::Existing(_)) && made.path.is_none() {
            tracing::warn!(whose = ?made.whose, "the account was gone before it could be saved");
            return false;
        }
        drop(form);
        self.ask(match made.whose {
            Whose::New => Ask::Create(made),
            Whose::Existing(_) => Ask::Save(made),
        });
        true
    }

    /// Take an account off the machine.
    pub fn remove(&self, uid: u64, files: bool) {
        let Some(person) = listing().person(uid).cloned() else {
            return;
        };
        self.ask(Ask::Remove {
            uid,
            path: person.path,
            files,
        });
    }

    fn ask(&self, ask: Ask) {
        let mut state = self.state();
        // Said to be working straight away rather than when the worker wakes:
        // the press has happened, polkit is about to ask for a password, and
        // the row has to stop offering to do it again in the meantime.
        state.listing.working = true;
        state.listing.trouble = None;
        state.published += 1;
        state.asks.push(ask);
        state.dirty = true;
        self.shared.signal.notify_one();
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Users {
    fn drop(&mut self) {
        let mut state = self.state();
        state.done = true;
        self.shared.signal.notify_one();
    }
}

struct Worker {
    shared: Arc<Shared>,
    /// The system bus, opened on the first pass that needs it. `None` on a
    /// machine with no D-Bus at all, where the page says there is no account
    /// service — which is true, because nothing could be reached to be one.
    bus: Option<zbus::blocking::Connection>,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self { shared, bus: None }
    }

    fn run(mut self) {
        while self.tick() {
            self.wait();
        }
    }

    /// One pass. `false` when the shell has gone away.
    fn tick(&mut self) -> bool {
        let (watching, asks) = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.done {
                return false;
            }
            state.dirty = false;
            (state.watching, std::mem::take(&mut state.asks))
        };
        if !watching && asks.is_empty() {
            return true;
        }
        self.connect();
        let mut trouble = None;
        let asked = !asks.is_empty();
        for ask in asks {
            if let Err(refused) = self.carry_out(ask) {
                tracing::warn!(
                    what = refused.what,
                    why = refused.why,
                    "the account change was refused"
                );
                trouble = Some(refused);
            }
        }
        // Read again after a change whether or not the page is open: the user
        // pressed a row and is owed the answer, and the page they pressed it on
        // is the one they are looking at.
        if watching || asked {
            let listing = self.read(trouble);
            self.publish(listing);
        }
        true
    }

    fn wait(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done || state.dirty {
            return;
        }
        if state.watching {
            drop(self.shared.signal.wait_timeout(state, REFRESH));
        } else {
            drop(self.shared.signal.wait(state));
        }
    }

    fn connect(&mut self) {
        if self.bus.is_some() {
            return;
        }
        match zbus::blocking::Connection::system() {
            Ok(bus) => self.bus = Some(bus),
            // Said at debug: a session with no system bus is one where this is
            // the expected answer every pass, and the page says it in words.
            Err(err) => tracing::debug!(?err, "no system bus; the Users page has nothing"),
        }
    }

    // --- reading -----------------------------------------------------------

    /// Every human account on this machine.
    fn read(&mut self, trouble: Option<Trouble>) -> Listing {
        let mut listing = Listing {
            trouble,
            ..Listing::default()
        };
        let Some(bus) = self.bus.as_ref() else {
            return listing;
        };
        let Some(paths) = list_users(bus) else {
            // The daemon is not there, or would not answer. Either way the page
            // has nothing to show and says so.
            return listing;
        };
        listing.daemon = true;
        let here = logged_in(bus);
        // SAFETY: `getuid` cannot fail and touches nothing.
        let ours = u64::from(unsafe { libc::getuid() });
        for path in paths {
            let Some(person) = read_person(bus, &path, &here, ours) else {
                continue;
            };
            listing.people.push(person);
        }
        // By user id, which is the order the accounts were made in — and the
        // only order that does not move under a cursor when somebody renames
        // themselves on the page they are standing on.
        listing.people.sort_by_key(|person| person.uid);
        listing
    }

    fn publish(&self, listing: Listing) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        // Nothing is in flight any more, whatever came of it.
        let settled = Listing {
            working: false,
            ..listing
        };
        if state.listing == settled {
            // Except that the *working* flag may have been the only difference,
            // and it is one the page is drawn from.
            if state.listing.working {
                state.listing.working = false;
                state.published += 1;
            }
            return;
        }
        state.listing = settled;
        state.published += 1;
    }

    // --- carrying out a press ----------------------------------------------

    fn carry_out(&mut self, ask: Ask) -> Result<(), Trouble> {
        let bus = self.bus.as_ref().ok_or_else(|| {
            Trouble::new(
                crate::i18n::text("shell-there-was-nothing-to-ask"),
                crate::i18n::text(
                    "shell-this-session-has-no-system-bus-so-no-account-service-could-be-reached",
                ),
            )
        })?;
        match ask {
            Ask::Create(made) => create(bus, made),
            Ask::Save(made) => save(bus, made),
            Ask::Remove { uid, path, files } => {
                tracing::info!(uid, files, "removing an account");
                // By object path as well as by id: `DeleteUser` takes the id,
                // and the path is what says the account we read is the account
                // being removed rather than whatever now holds that id.
                let _ = path;
                let uid = i64::try_from(uid).map_err(|_| {
                    Trouble::new(
                        crate::i18n::text("shell-that-account-cannot-be-removed"),
                        crate::i18n::text(
                            "shell-its-user-id-is-larger-than-the-account-service-will-take",
                        ),
                    )
                })?;
                change::<_, ()>(
                    bus,
                    ACCOUNTS_PATH,
                    ACCOUNTS_IFACE,
                    "DeleteUser",
                    &(uid, files),
                )
                .map_err(|why| {
                    Trouble::new(
                        crate::i18n::text("shell-the-account-could-not-be-removed"),
                        why,
                    )
                })
            }
        }
    }
}

/// Make an account, then put on it everything `CreateUser` does not take.
///
/// The order matters and is not arbitrary. The account has to exist before its
/// password or its picture can be set, and it is created as the *kind* it will
/// be rather than created and then promoted: a machine that lost power between
/// the two would be left with an account somebody meant to be an administrator
/// and is not, which is the failure that cannot be seen by looking at the page.
fn create(bus: &zbus::blocking::Connection, made: Made) -> Result<(), Trouble> {
    tracing::info!(name = made.name, admin = made.admin, "making an account");
    let kind = match made.admin {
        true => ADMINISTRATOR,
        false => STANDARD,
    };
    let path: OwnedObjectPath = change(
        bus,
        ACCOUNTS_PATH,
        ACCOUNTS_IFACE,
        "CreateUser",
        &(made.name.as_str(), made.real.as_str(), kind),
    )
    .map_err(|why| {
        Trouble::new(
            crate::i18n::text("shell-the-account-could-not-be-created"),
            why,
        )
    })?;
    let path = path.as_str().to_string();

    // The password. A new account has one by construction — the form will not
    // hand itself over without — so a failure here leaves an account nobody can
    // log in to, and it is said plainly rather than swallowed.
    set_password(bus, &path, &made.password)?;
    if made.chose_picture {
        write_picture(bus, &path, made.picture.as_deref())?;
    }
    Ok(())
}

/// Save a form about an account that already exists.
///
/// Only what really changed is written, and that is not tidiness: every one of
/// these is a separate polkit action, and a save that set all five would ask the
/// machine's policy five questions where the user changed one thing. On a
/// machine whose policy wants a password each time, that is five panels.
fn save(bus: &zbus::blocking::Connection, made: Made) -> Result<(), Trouble> {
    let path = made.path.as_deref().ok_or_else(|| {
        Trouble::new(
            crate::i18n::text("shell-that-account-has-gone"),
            crate::i18n::text("shell-it-was-taken-off-this-machine-while-the-form-was-open"),
        )
    })?;
    let was = made.was.as_ref();
    tracing::info!(name = made.name, "saving an account");

    if was.is_none_or(|was| was.real != made.real) {
        change::<_, ()>(bus, path, USER_IFACE, "SetRealName", &(made.real.as_str(),)).map_err(
            |why| {
                Trouble::new(
                    crate::i18n::text("shell-the-name-could-not-be-changed"),
                    why,
                )
            },
        )?;
    }
    if was.is_none_or(|was| was.name != made.name) {
        change::<_, ()>(bus, path, USER_IFACE, "SetUserName", &(made.name.as_str(),)).map_err(
            |why| {
                Trouble::new(
                    crate::i18n::text("shell-the-user-name-could-not-be-changed"),
                    why,
                )
            },
        )?;
    }
    if was.is_none_or(|was| was.admin != made.admin) {
        let kind = match made.admin {
            true => ADMINISTRATOR,
            false => STANDARD,
        };
        change::<_, ()>(bus, path, USER_IFACE, "SetAccountType", &(kind,)).map_err(|why| {
            Trouble::new(
                crate::i18n::text("shell-the-account-type-could-not-be-changed"),
                why,
            )
        })?;
    }
    // An empty password is "leave it alone" on an account that already has one
    // — see [`fault_in_password`], which is where that is decided and said.
    if !made.password.is_empty() {
        set_password(bus, path, &made.password)?;
    }
    if made.chose_picture {
        write_picture(bus, path, made.picture.as_deref())?;
    }
    Ok(())
}

/// Hash the password and hand over the hash.
///
/// The one place in this module where the plaintext exists as text, and it
/// exists there for the length of one closure: [`Secret::as_text`] lends it, the
/// hash is computed inside, and what comes back out is the hash. Nothing copies
/// it into a `String` on the way — see that method's own note, which is where
/// the rule is written down.
fn set_password(
    bus: &zbus::blocking::Connection,
    path: &str,
    password: &Secret,
) -> Result<(), Trouble> {
    let hashed = password
        .as_text(crate::crypt::hash)
        .flatten()
        .ok_or_else(|| {
            Trouble::new(
                crate::i18n::text("shell-the-password-could-not-be-set"),
                crate::i18n::text("users-password-no-randomness"),
            )
        })?;
    // No hint. `accounts-daemon` puts one on the login screen for anybody to
    // read, and a hint typed on a form beside the password it is about is one
    // people fill in with the password.
    change::<_, ()>(bus, path, USER_IFACE, "SetPassword", &(hashed.as_str(), "")).map_err(|why| {
        Trouble::new(
            crate::i18n::text("shell-the-password-could-not-be-set"),
            why,
        )
    })
}

/// Point the account at a picture, or take the one it has away.
fn write_picture(
    bus: &zbus::blocking::Connection,
    path: &str,
    picture: Option<&Path>,
) -> Result<(), Trouble> {
    // The empty string is how `SetIconFile` is told there is to be no picture.
    let file = picture.and_then(Path::to_str).unwrap_or_default();
    change::<_, ()>(bus, path, USER_IFACE, "SetIconFile", &(file,))
        .map_err(|why| Trouble::new(crate::i18n::text("shell-the-avatar-could-not-be-set"), why))
}

fn list_users(bus: &zbus::blocking::Connection) -> Option<Vec<String>> {
    let reply = call(bus, ACCOUNTS_PATH, ACCOUNTS_IFACE, "ListCachedUsers", &()).ok()?;
    let paths: Vec<OwnedObjectPath> = reply.body().deserialize().ok()?;
    Some(paths.into_iter().map(|p| p.as_str().to_string()).collect())
}

/// One account, out of the properties of its object.
fn read_person(
    bus: &zbus::blocking::Connection,
    path: &str,
    here: &[u64],
    ours: u64,
) -> Option<Person> {
    let properties = get_all(bus, path, USER_IFACE)?;
    // A service account has no business on this page. `ListCachedUsers` leaves
    // them out already; this is the second opinion, because the daemon's idea
    // of cached has changed between versions and a page that listed `nobody`
    // would be offering to give it a password.
    if flag(&properties, "SystemAccount").unwrap_or(false) {
        return None;
    }
    let uid = number(&properties, "Uid")?;
    let name = text(&properties, "UserName")?;
    Some(Person {
        uid,
        path: path.to_string(),
        real: text(&properties, "RealName").unwrap_or_default(),
        admin: number(&properties, "AccountType").unwrap_or(0) == ADMINISTRATOR as u64,
        stamp: text(&properties, "IconFile")
            .map(PathBuf::from)
            .filter(|file| !file.as_os_str().is_empty())
            .and_then(|file| std::fs::metadata(file).ok()?.modified().ok())
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs()),
        picture: text(&properties, "IconFile")
            .filter(|file| !file.is_empty())
            .map(PathBuf::from)
            // Checked, because `IconFile` goes on naming a file somebody
            // deleted — see [`Person::picture`].
            .filter(|file| file.is_file()),
        home: text(&properties, "HomeDirectory")
            .map(PathBuf::from)
            .unwrap_or_default(),
        here: here.contains(&uid),
        you: uid == ours,
        name,
    })
}

/// Which accounts have a session on this machine right now.
///
/// `logind` rather than `utmp`, because `logind` is what a Wayland session runs
/// under and `utmp` is what a login on a terminal writes: a user logged in
/// graphically on another seat is in the first and not always in the second, and
/// removing their account out from under them is exactly what this is here to
/// prevent.
///
/// An empty answer where `logind` cannot be reached, which is the *unsafe*
/// direction — it would let a removal be offered for somebody who is signed in.
/// It is the honest one all the same: the daemon itself refuses to remove an
/// account with a session, so what is lost is the explanation, not the guard.
fn logged_in(bus: &zbus::blocking::Connection) -> Vec<u64> {
    let Ok(reply) = bus.call_method(Some(LOGIN), LOGIN_PATH, Some(LOGIN_IFACE), "ListUsers", &())
    else {
        tracing::debug!("logind would not say who is logged in");
        return Vec::new();
    };
    // `a(uso)`: the user id, the name, and the object for their session.
    let Ok(users) = reply
        .body()
        .deserialize::<Vec<(u32, String, OwnedObjectPath)>>()
    else {
        return Vec::new();
    };
    users.into_iter().map(|(uid, ..)| u64::from(uid)).collect()
}

// --- talking to the bus ----------------------------------------------------

/// One call that *changes* an account, made in a way that lets polkit ask.
///
/// The whole of the difference is one bit in the message header, and it cost
/// two days to find. `accounts-daemon` does not decide on its own whether a
/// password may be asked for: it reads the D-Bus
/// `ALLOW_INTERACTIVE_AUTHORIZATION` flag off the call and passes it to polkit
/// as `allow_user_interaction`. Without it, polkit answers "a challenge is
/// required" and **never contacts an authentication agent at all** — not
/// because none is registered, but because the caller has said it is not
/// prepared to wait for one. The daemon turns that into
/// `PermissionDenied: Authentication is required`, in about seven
/// milliseconds, and every panel in the shell stays exactly where it was.
///
/// GLib sets the flag for anybody using `GDBusProxy`, which is why every
/// desktop written against it has never had to know this. zbus does not set it,
/// and there is no reason it should: a program that is not going to wait must
/// not claim it will.
///
/// So this is the door every change goes through, and [`call`] — which sets no
/// flag — is left for reading, where there is nothing to authorise and nothing
/// to wait for.
fn change<Body, Reply>(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &Body,
) -> Result<Reply, String>
where
    Body: serde::ser::Serialize + zbus::zvariant::DynamicType,
    Reply: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let proxy =
        zbus::blocking::Proxy::new(bus, ACCOUNTS, path, interface).map_err(|err| why(&err))?;
    // Taken before the call rather than after the failure, so what it counts is
    // this call and not somebody else's: two presses cannot overlap here — the
    // worker carries one ask at a time — but a share question or a mounted disk
    // can raise a polkit question of its own at any moment.
    let asked = crate::polkit::asked_so_far();
    let answered = proxy.call_with_flags::<_, _, Reply>(
        method,
        zbus::proxy::MethodFlags::AllowInteractiveAuth.into(),
        body,
    );
    match answered {
        // `None` is only ever the answer to a call that said it wanted no
        // reply, and this one does not say that.
        Ok(Some(reply)) => Ok(reply),
        Ok(None) => {
            Err(crate::i18n::text("shell-the-account-service-answered-nothing-at-all").to_string())
        }
        Err(err) => {
            tracing::warn!(path, interface, method, ?err, "the account service refused");
            let mut why = why(&err);
            // The whole of the difference between a user who said no and a
            // session that was never asked. `accounts-daemon` says
            // "Authentication is required" for both, and a page that repeated
            // only that sent somebody looking for a password panel that had
            // never been raised and never would be.
            if wanted_a_password(&err) && crate::polkit::asked_so_far() == asked {
                why.push_str(match crate::polkit::holds_the_session() {
                    // The shell *is* this session's panel and was still not
                    // asked, which is not about this page at all: whatever
                    // decided is between the daemon and polkitd.
                    true => " This session holds polkit's password panel and was still never asked for one.",
                    // And this one is, and covers both ways of getting here:
                    // something else took the slot, and there being no session
                    // to take one for.
                    false => " This shell is not this session's polkit agent, so polkitd never gave it the question.",
                });
            }
            Err(why)
        }
    }
}

/// One call that only *reads*, with what the daemon said when it would not.
///
/// A refusal is never a reason to take the session down — polkit declining a
/// change is ordinary — but it *is* a reason the page has to be able to give.
/// This used to answer `None` and write the error at `debug`, and the cost of
/// that was a real morning: a password that would not be set, a page that said
/// "That did not work", and no way from either end to find out that the daemon
/// had been saying why the whole time. The reading side still throws it away
/// with `.ok()`, because a listing that cannot be read is its own empty page.
/// Nothing that *changes* an account may.
fn call<Body>(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &Body,
) -> Result<zbus::Message, String>
where
    Body: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    match bus.call_method(Some(ACCOUNTS), path, Some(interface), method, body) {
        Ok(reply) => Ok(reply),
        Err(err) => {
            // At `warn`, not `debug`. Somebody reading a journal to find out
            // why a press did nothing is reading it at the level a refusal is
            // written at, and this one was two levels below that.
            tracing::warn!(path, interface, method, ?err, "the account service refused");
            Err(why(&err))
        }
    }
}

/// Whether this refusal was about proving who you are.
///
/// `accounts-daemon` answers `PermissionDenied` for every one of them — the
/// policy saying no, the user pressing Cancel, and nobody being asked at all —
/// so the name is what says it was polkit rather than the account, and no more
/// than that.
fn wanted_a_password(err: &zbus::Error) -> bool {
    let zbus::Error::MethodError(name, ..) = err else {
        return false;
    };
    matches!(
        name.as_str().rsplit('.').next(),
        Some("PermissionDenied" | "NotAuthorized")
    )
}

/// The daemon's own words, out of a D-Bus error and fit to put on a row.
///
/// A `MethodError` carries two things: the error's name, which is a Java-shaped
/// string nobody should be shown, and the message the daemon wrote, which is
/// often the exact sentence somebody needs — `usermod` saying an account is
/// busy, or polkit saying it was not authorised. The message when there is one;
/// the last word of the name when there is not, which at least says *what kind*
/// of refusal it was; and the error itself for the failures that never reached
/// the daemon at all.
fn why(err: &zbus::Error) -> String {
    if let zbus::Error::MethodError(name, detail, _) = err {
        if let Some(detail) = detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            // The daemon ends some of these with a full stop and some without,
            // and the row reads as one sentence either way.
            return match detail.ends_with(['.', '!', '?']) {
                true => detail.to_string(),
                false => format!("{detail}."),
            };
        }
        let name = name.as_str();
        return match name.rsplit('.').next().filter(|tail| !tail.is_empty()) {
            Some(tail) => {
                crate::message!("account-service-answered", "answer" => tail)
            }
            None => {
                crate::message!("account-service-answered", "answer" => name)
            }
        };
    }
    format!("{err}")
}

fn get_all(
    bus: &zbus::blocking::Connection,
    path: &str,
    interface: &str,
) -> Option<HashMap<String, OwnedValue>> {
    call(bus, path, PROPERTIES, "GetAll", &(interface,))
        .ok()?
        .body()
        .deserialize()
        .ok()
}

fn number(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u64> {
    let value = properties.get(name)?;
    // Through every width the daemon uses for a small number: `Uid` is a `t`
    // and `AccountType` is an `i`, and the page does not care which.
    u64::try_from(value)
        .or_else(|_| u32::try_from(value).map(u64::from))
        .or_else(|_| i32::try_from(value).map(|number| number.max(0) as u64))
        .or_else(|_| i64::try_from(value).map(|number| number.max(0) as u64))
        .ok()
}

fn flag(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    bool::try_from(properties.get(name)?).ok()
}

fn text(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    String::try_from(properties.get(name)?.try_clone().ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(uid: u64, name: &str, admin: bool) -> Person {
        Person {
            uid,
            path: format!("/org/freedesktop/Accounts/User{uid}"),
            name: name.to_string(),
            real: String::new(),
            admin,
            picture: None,
            stamp: None,
            home: PathBuf::from(format!("/home/{name}")),
            here: false,
            you: false,
        }
    }

    fn listing(people: Vec<Person>) -> Listing {
        Listing {
            daemon: true,
            people,
            trouble: None,
            working: false,
        }
    }

    /// Nothing on this machine is called this. Used where a rule has to be
    /// tested against a name that is free, without asserting what *is* taken.
    fn free(_: &str) -> Option<u64> {
        None
    }

    use super::one_form_at_a_time as alone;

    /// The shape of a user name, which is the shell's own rule and the one
    /// thing on this page that is refused before anything else is asked.
    ///
    /// It is deliberately narrower than what `useradd --badname` would take. A
    /// name outside it is refused here, with a sentence, rather than three
    /// layers down by a tool whose refusal would reach the user as a press that
    /// did nothing.
    #[test]
    fn a_user_name_has_to_be_one_linux_will_take() {
        let ok = |name: &str| fault_in_name_among(Whose::New, name, free);
        assert_eq!(ok("marta"), None);
        assert_eq!(ok("marta-k"), None);
        assert_eq!(ok("marta_2"), None);
        assert_eq!(ok("_service"), None);
        // Trimmed before it is judged, so a space picked up from the on-screen
        // keyboard is not a name nobody can explain.
        assert_eq!(ok("  marta  "), None);

        assert!(ok("").is_some(), "an empty name");
        assert!(ok("2fast").is_some(), "starting with a digit");
        assert!(ok("-marta").is_some(), "starting with a hyphen");
        assert!(ok("Marta").is_some(), "an upper-case letter");
        assert!(ok("mar ta").is_some(), "a space inside");
        assert!(ok("mar:ta").is_some(), "a colon, which ends a passwd field");
        assert!(ok("mar/ta").is_some(), "a slash, which is a path");
        assert!(ok("marté").is_some(), "beyond ASCII");
        assert!(ok(&"m".repeat(33)).is_some(), "longer than useradd takes");
        assert_eq!(ok(&"m".repeat(32)), None, "exactly as long as it takes");
    }

    /// A name somebody already has is refused — except on the account that
    /// already has it, which is what somebody editing their own account and
    /// leaving the name alone has typed.
    #[test]
    fn a_user_name_already_taken_is_only_free_for_its_own_account() {
        let taken = |name: &str| (name == "marta").then_some(1001);
        assert!(fault_in_name_among(Whose::New, "marta", taken).is_some());
        assert!(fault_in_name_among(Whose::Existing(1002), "marta", taken).is_some());
        assert_eq!(
            fault_in_name_among(Whose::Existing(1001), "marta", taken),
            None
        );
        // And a name nobody has is free for either.
        assert_eq!(fault_in_name_among(Whose::New, "jan", taken), None);
    }

    /// A new account needs a password and an existing one does not: an empty
    /// field on a form about somebody who already has one means "leave it
    /// alone", which is what a user who came to change the picture has typed.
    #[test]
    fn a_password_is_needed_for_a_new_account_and_optional_for_an_old_one() {
        assert!(fault_in_password(Whose::New, "").is_some());
        assert_eq!(fault_in_password(Whose::Existing(1001), ""), None);
        // And no length is refused. There was a six-character floor here once;
        // it was a rule about somebody else's machine and it is gone. What is
        // left is a field, not a policy.
        assert_eq!(fault_in_password(Whose::New, "a"), None, "one character");
        assert_eq!(fault_in_password(Whose::New, "hi"), None, "two");
        assert_eq!(fault_in_password(Whose::New, "sixchr"), None);
        assert_eq!(fault_in_password(Whose::Existing(1001), "sixchr"), None);
        // Too long to fit in what holds it. A password silently cut is one
        // nobody can log in with.
        assert!(fault_in_password(Whose::New, &"x".repeat(256)).is_some());
    }

    /// Somebody signed in cannot be renamed, and the page says so itself.
    ///
    /// `usermod -l` refuses to rename an account with a running process, so
    /// this is not a policy but a fact about the machine — and one that used to
    /// arrive as a saved form that had not saved. Only a *change* is refused:
    /// the same name back is not a rename.
    #[test]
    fn an_account_being_used_cannot_be_renamed() {
        let _alone = one_form_at_a_time();
        note(listing(vec![
            Person {
                here: true,
                you: true,
                ..person(1000, "marta", true)
            },
            Person {
                here: true,
                ..person(1001, "jan", false)
            },
            person(1002, "ola", false),
        ]));

        // You, which is the case somebody hits first: it is their own account
        // they are standing in.
        assert!(fault_in_rename(Whose::Existing(1000), "marta2").is_some());
        assert_eq!(fault_in_rename(Whose::Existing(1000), "marta"), None);
        assert_eq!(
            fault_in_rename(Whose::Existing(1000), "  marta  "),
            None,
            "the field is trimmed before it is compared"
        );

        // Somebody else, signed in on another seat. A different sentence,
        // because what has to happen next is somebody else's doing.
        let theirs = fault_in_rename(Whose::Existing(1001), "jan2");
        assert!(theirs.is_some_and(|fault| fault.contains("sign out")));

        // And an account nobody is using renames perfectly well.
        assert_eq!(fault_in_rename(Whose::Existing(1002), "ola2"), None);
        // As does one that does not exist yet.
        assert_eq!(fault_in_rename(Whose::New, "anybody"), None);

        // It is on the field the panel asks about, not only on the row at the
        // foot: a panel that took the name and refused it a column later would
        // be the shell knowing and not saying.
        assert!(fault(Whose::Existing(1000), Field::Username, "marta2").is_some());
    }

    /// A rewritten avatar is a different listing, even at the same path.
    ///
    /// The whole of what makes a changed avatar reach the screen. An account's
    /// picture lives at one fixed name for the life of the account, so the path
    /// says nothing about which picture it is; the stamp does, it is part of
    /// what makes two listings differ, and a listing that differs is one the
    /// page is rebuilt from — which is where the drawn copy is thrown away.
    /// Without it somebody wears the face they had when the session started.
    #[test]
    fn a_new_avatar_at_the_same_path_is_a_change() {
        let face = PathBuf::from("/var/lib/AccountsService/icons/marta");
        let before = Person {
            picture: Some(face.clone()),
            stamp: Some(1_000),
            ..person(1000, "marta", true)
        };
        let after = Person {
            stamp: Some(2_000),
            ..before.clone()
        };
        assert_ne!(before, after, "the same path, a different picture");
        assert_ne!(
            listing(vec![before.clone()]),
            listing(vec![after]),
            "and so a different listing, which is what republishes"
        );

        // And an account whose picture has not moved is not a change, or the
        // page would rebuild itself every five seconds for ever.
        assert_eq!(listing(vec![before.clone()]), listing(vec![before]));
    }

    /// Every change carries the one bit that lets polkit ask for a password.
    ///
    /// Pinned against the wire, not against zbus: the D-Bus specification gives
    /// `ALLOW_INTERACTIVE_AUTHORIZATION` the value 4 in the header's flags
    /// byte, and that number is what `accounts-daemon` reads. Without it polkit
    /// answers "a challenge is required" and never contacts an agent, the
    /// daemon says `Authentication is required`, and no panel is raised — the
    /// bug this whole page was unusable for. See [`change`].
    #[test]
    fn a_change_says_it_will_wait_for_a_password() {
        assert_eq!(zbus::message::Flags::AllowInteractiveAuth as u8, 4);
        assert_eq!(zbus::proxy::MethodFlags::AllowInteractiveAuth as u32, 4);

        // And a message built with it really carries it, which is the half of
        // this that belongs to zbus rather than to the specification.
        let asking = zbus::Message::method_call("/org/freedesktop/Accounts", "CreateUser")
            .expect("a well-formed call")
            .with_flags(zbus::message::Flags::AllowInteractiveAuth)
            .expect("a flag a method call may carry")
            .build(&())
            .expect("a well-formed call");
        assert!(asking
            .primary_header()
            .flags()
            .contains(zbus::message::Flags::AllowInteractiveAuth));

        // And that a plain call does not, which is what the reading side sends
        // and what every change used to.
        let silent = zbus::Message::method_call("/org/freedesktop/Accounts", "ListCachedUsers")
            .expect("a well-formed call")
            .build(&())
            .expect("a well-formed call");
        assert!(!silent
            .primary_header()
            .flags()
            .contains(zbus::message::Flags::AllowInteractiveAuth));
    }

    /// What the daemon said comes out of the error and onto the row.
    #[test]
    fn a_refusal_keeps_the_words_the_machine_used() {
        let refused = zbus::Error::MethodError(
            zbus::names::OwnedErrorName::try_from(
                "org.freedesktop.Accounts.Error.PermissionDenied",
            )
            .expect("a well-formed error name"),
            Some("Not authorized".to_string()),
            zbus::Message::method_call("/org/freedesktop/Accounts", "Whatever")
                .expect("a well-formed call")
                .build(&())
                .expect("a well-formed call"),
        );
        // Given a full stop, because the row reads it as a sentence and the
        // daemon does not always end one.
        assert_eq!(why(&refused), "Not authorized.");

        // An error with nothing written on it still says what kind it was,
        // which is more than a page that said nothing at all.
        let silent = zbus::Error::MethodError(
            zbus::names::OwnedErrorName::try_from("org.freedesktop.Accounts.Error.Failed")
                .expect("a well-formed error name"),
            None,
            zbus::Message::method_call("/org/freedesktop/Accounts", "Whatever")
                .expect("a well-formed call")
                .build(&())
                .expect("a well-formed call"),
        );
        assert_eq!(why(&silent), "The account service answered Failed.");

        // And which of them was polkit's. Only the first: the second is the
        // daemon failing at something, and a note about password panels on it
        // would send somebody looking in the wrong place.
        assert!(wanted_a_password(&refused));
        assert!(!wanted_a_password(&silent));
        assert!(
            !wanted_a_password(&zbus::Error::InvalidReply),
            "and a failure that never reached the daemon is not about a password"
        );
    }

    /// A person's own name may be empty — a great many accounts have none — but
    /// it may not carry the one character that would end its field early.
    #[test]
    fn a_real_name_may_be_empty_but_not_break_the_passwd_file() {
        assert_eq!(fault_in_real_name(""), None);
        assert_eq!(fault_in_real_name("Marta Kowalska"), None);
        assert!(fault_in_real_name("Marta: Kowalska").is_some());
        assert!(fault_in_real_name("Marta\nKowalska").is_some());
        assert!(fault_in_real_name(&"m".repeat(256)).is_some());
    }

    /// The last administrator is the one account whose type cannot be changed
    /// and which cannot be removed — because nothing in this shell could put a
    /// machine with no administrator right afterwards.
    #[test]
    fn the_only_administrator_is_recognised_as_such() {
        let one = listing(vec![
            person(1000, "marta", true),
            person(1001, "jan", false),
        ]);
        assert!(one.only_admin(1000));
        assert!(
            !one.only_admin(1001),
            "a standard account is not the last admin"
        );

        let two = listing(vec![person(1000, "marta", true), person(1001, "jan", true)]);
        assert!(!two.only_admin(1000));
        assert!(!two.only_admin(1001));
        assert_eq!(two.admins(), 2);

        // An account that is not on this machine at all is not its last
        // administrator either, which is the answer a row rebuilt around a
        // deleted account needs.
        assert!(!one.only_admin(4242));
    }

    /// A row says who logs in, what they may do, and whether they are here —
    /// and it is titled by the name they chose, falling back to the name they
    /// log in as for an account nobody has named.
    #[test]
    fn a_row_says_who_they_are_and_what_they_may_do() {
        let mut marta = person(1000, "marta", true);
        assert_eq!(marta.title(), "marta", "no real name, so the user name");
        assert_eq!(marta.note(), "marta — Administrator");

        marta.real = "Marta Kowalska".to_string();
        assert_eq!(marta.title(), "Marta Kowalska");
        assert_eq!(marta.note(), "marta — Administrator");

        marta.here = true;
        assert_eq!(marta.note(), "marta — Administrator — signed in");
        // "You" rather than "signed in" for this session's own account: both
        // are true, and the one that decides what the page will let them do is
        // the second.
        marta.you = true;
        assert_eq!(marta.note(), "marta — Administrator — you");

        let jan = person(1001, "jan", false);
        assert_eq!(jan.note(), "jan — Standard");
    }

    /// A form opens holding what the account *is*, so a user who came to change
    /// one thing leaves the rest alone by doing nothing — and opening it again
    /// on the same account does not throw away what has been typed since.
    ///
    /// The second half is what makes it safe to ask once a frame, which is how
    /// the shell notices the cursor stepping in. See `Shell::sync_user_form`.
    #[test]
    fn a_form_opens_from_the_account_and_is_not_reopened_underneath_the_typing() {
        let _alone = alone();
        let mut marta = person(1000, "marta", true);
        marta.real = "Marta Kowalska".to_string();
        marta.picture = Some(PathBuf::from("/var/lib/AccountsService/icons/marta"));

        open_form(Whose::Existing(1000), Some(&marta));
        let opened = form().expect("a form is open");
        assert_eq!(opened.whose, Whose::Existing(1000));
        assert_eq!(opened.real, "Marta Kowalska");
        assert_eq!(opened.name, "marta");
        assert!(opened.admin);
        assert_eq!(
            opened.picture.as_deref(),
            Some(marta.picture.as_ref().unwrap().as_path())
        );
        assert!(!opened.chose_picture, "nothing chosen on the form yet");
        assert_eq!((opened.typed, opened.confirmed), (0, 0));

        // Type into it, then let the once-a-frame watch ask again.
        write(Field::Name, "Marta K");
        set_admin(false);
        open_form(Whose::Existing(1000), Some(&marta));
        let again = form().expect("still open");
        assert_eq!(again.real, "Marta K", "the typing was thrown away");
        assert!(!again.admin, "the choice was thrown away");

        // A form about a *different* account replaces it, because it is a
        // different form.
        open_form(Whose::New, None);
        let fresh = form().expect("a new form");
        assert_eq!(fresh.whose, Whose::New);
        assert_eq!(fresh.real, "");
        assert!(
            !fresh.admin,
            "a new account is Standard until somebody says so"
        );

        // And `form_for` answers only about the form that is open, which is
        // what lets every row of every form ask whether it is the live one.
        assert!(form_for(Whose::New).is_some());
        assert!(form_for(Whose::Existing(1000)).is_none());

        close_form();
        assert!(form().is_none());
    }

    /// The row at the foot of a form says what is missing, and says nothing
    /// only when the form really is an account.
    ///
    /// Asked of the whole form rather than of one field, because a field is
    /// only checked when somebody presses Set on its panel — and a form can be
    /// filled in by pressing nothing at all and walking to the bottom.
    #[test]
    fn a_form_is_not_handed_over_until_it_describes_an_account() {
        let _alone = alone();
        open_form(Whose::New, None);
        assert!(fault_in_form_for(Whose::New).is_some(), "an empty form");

        write(Field::Username, "marta");
        assert!(fault_in_form_for(Whose::New).is_some(), "still no password");

        let secret = |text: &str| {
            let mut secret = Secret::default();
            for character in text.chars() {
                secret.push(character);
            }
            secret
        };
        write_secret(Field::Password, secret("hunter2"));
        assert!(
            fault_in_form_for(Whose::New).is_some(),
            "nothing confirms it"
        );

        write_secret(Field::Confirm, secret("hunter3"));
        assert!(
            fault_in_form_for(Whose::New).is_some(),
            "the two do not agree"
        );

        write_secret(Field::Confirm, secret("hunter2"));
        assert_eq!(
            fault_in_form_for(Whose::New),
            None,
            "a name and two matching passwords"
        );

        // A name that is not a name puts it back, whatever the passwords say.
        write(Field::Name, "Marta: Kowalska");
        assert!(fault_in_form_for(Whose::New).is_some());
        write(Field::Name, "Marta Kowalska");
        assert_eq!(fault_in_form_for(Whose::New), None);

        // A picture that has gone between being chosen and being pressed.
        set_picture(Path::new("/definitely/not/here.png"));
        assert!(
            fault_in_form_for(Whose::New).is_some(),
            "the picture is not there"
        );
        drop_picture();
        assert_eq!(
            fault_in_form_for(Whose::New),
            None,
            "and taking it off puts it back"
        );

        // With the form gone the row falls back to what an unstarted one says,
        // which for a new account is that it is missing everything.
        close_form();
        assert!(fault_in_form_for(Whose::New).is_some(), "an unstarted form");
        assert_eq!(
            fault_in_form_for(Whose::Existing(1000)),
            None,
            "an account that already exists is already an account"
        );
    }

    /// A password never becomes a `String`, and the row that stands for it says
    /// how many characters are waiting rather than what they are.
    ///
    /// The check is that [`write`] refuses to take one: the two are separate
    /// entry points on purpose, and a password that had been through a `&str`
    /// would be a copy of it in an allocation nothing overwrites.
    #[test]
    fn a_password_cannot_be_written_down_as_text() {
        let _alone = alone();
        open_form(Whose::New, None);
        write(Field::Password, "hunter2");
        let refused = form().expect("a form");
        assert_eq!(refused.typed, 0, "it was refused rather than stored");
        assert_eq!(refused.real, "", "and it did not land in another field");

        let mut secret = Secret::default();
        for character in "hunter2".chars() {
            secret.push(character);
        }
        write_secret(Field::Password, secret);
        assert_eq!(form().expect("a form").typed, 7);
        close_form();
    }

    /// Both password fields are drawn as a count of marks and every other typed
    /// row is drawn as what was typed — which is the whole of what tells the
    /// panel which kind of field to raise.
    #[test]
    fn only_the_passwords_are_secret() {
        assert!(!Field::Name.secret());
        assert!(!Field::Username.secret());
        assert!(Field::Password.secret());
        assert!(Field::Confirm.secret());
        // And every field says what to type, because on a television an empty
        // well with no sentence over it is a question nobody can answer.
        for field in [
            Field::Name,
            Field::Username,
            Field::Password,
            Field::Confirm,
        ] {
            assert!(!field.title().is_empty());
            assert!(field.note().len() > 20, "{}", field.title());
        }
    }

    /// Every call this module makes is the shape `accounts-daemon` advertises.
    ///
    /// The one part of talking to the daemon that cannot be checked by running
    /// it: a body of the wrong shape is refused at the far end, at the moment
    /// somebody presses the row, with an error that reaches the page as "the
    /// account could not be created" and says nothing about why. So the shapes
    /// are pinned here against what `busctl introspect
    /// org.freedesktop.Accounts` prints, and a call whose arguments are changed
    /// without its signature being thought about fails the build instead.
    ///
    /// zbus makes the message body out of the value it is handed, so the outer
    /// brackets below are the body itself rather than a struct inside it —
    /// `(ssi)` here is the daemon's `ssi`.
    #[test]
    fn every_call_is_the_shape_the_daemon_advertises() {
        use zbus::zvariant::DynamicType;
        let shape = |body: &dyn DynamicType| body.signature().to_string();

        // org.freedesktop.Accounts
        assert_eq!(shape(&("marta", "Marta Kowalska", ADMINISTRATOR)), "(ssi)");
        assert_eq!(shape(&(1000i64, true)), "(xb)");
        assert_eq!(shape(&()), "");

        // org.freedesktop.Accounts.User — the hash and its hint, the picture,
        // the two names, and the kind of account.
        assert_eq!(shape(&("$6$salt$hash", "")), "(ss)");
        assert_eq!(shape(&("/var/lib/AccountsService/icons/marta",)), "(s)");
        assert_eq!(shape(&("Marta Kowalska",)), "(s)");
        assert_eq!(shape(&(STANDARD,)), "(i)");

        // org.freedesktop.login1.Manager.ListUsers answers `a(uso)`, which is
        // what says who may not be removed.
        assert_eq!(
            shape(&vec![(
                1000u32,
                "marta".to_string(),
                OwnedObjectPath::default()
            )]),
            "a(uso)"
        );

        // And the two kinds of account really are the numbers the daemon uses
        // for them, which nothing else in this module would notice being wrong:
        // an account created as the wrong kind looks exactly like one somebody
        // meant to create.
        assert_eq!((STANDARD, ADMINISTRATOR), (0, 1));
    }

    /// A machine with no account service says so rather than showing an empty
    /// page — the same bargain the Network page states.
    #[test]
    fn a_machine_with_no_account_service_has_an_empty_listing() {
        let none = Listing::default();
        assert!(!none.daemon);
        assert!(none.people.is_empty());
        assert_eq!(none.admins(), 0);
        assert!(!none.only_admin(1000));
        assert!(none.person(1000).is_none());
    }
}
