//! Steam, as the shell holds it: one account, one panel, one column.
//!
//! The client itself is a crate of its own — see `lxb-steam` — because none of
//! what it does may happen on the thread that draws. This is the half that
//! belongs to the shell: what is on screen while somebody signs in, what has
//! been typed into it, and the rows the library turns into.
//!
//! ## The sign-in, as a panel
//!
//! Signing in is a conversation, and the panel is one question at a time. What
//! makes it a state machine rather than a screen is that Steam decides which
//! questions there are: an account with a phone authenticator is confirmed by
//! pressing something on the phone, one without is confirmed by a code from an
//! email, and a sign-in by photographed code skips the whole of it. So the
//! shell asks the client what it is waiting for and draws that, and the stages
//! below are the complete list of things it can be waiting for.
//!
//! ```text
//!                    ┌──────────► Qr ──────────┐
//!   press Steam ─► Choosing                    ├──► Waiting ──► signed in
//!                    └─► Account ─► Password ──┘        │
//!                                                       ▼
//!                                                     Code ──► Waiting
//! ```
//!
//! Every stage can be cancelled, and any of them can end in [`Stage::Failed`],
//! which is the only stage that offers to start again — everything else offers
//! to give up, because a panel that could be dismissed *into* another attempt
//! is a panel a user cannot leave.
//!
//! ## Why the password is the shell's own type
//!
//! [`crate::secret::Secret`] is what collects it, exactly as it does for the
//! uninstall panel and for polkit, and it hands itself over the one way it
//! knows how: by writing itself down a sink. The sink here is
//! [`lxb_steam::Password`], which is the client crate's own overwritten
//! buffer. So the password goes field → sink → RSA, is never a `String`, is
//! never in the layout, and both halves of the journey are held by a type
//! whose whole job is to overwrite itself afterwards.

use std::collections::{BTreeMap, BTreeSet};

use lxb_steam::{Confirmation, Doing, Event, Game, Stopped};

use crate::dialog;
use crate::menu;
use crate::secret::Secret;

/// Steam as one field of the shell.
pub struct Steam {
    client: lxb_steam::Steam,
    /// Who is signed in, if anybody. The shell's echo of what the worker last
    /// said, because the Games row is drawn from it on every rebuild.
    account: Option<String>,
    /// The library as it stands, in the order it arrives — installed first,
    /// each half by name. What order it goes on the *bar* in is `sort`, and
    /// [`Steam::rows`] is where the two meet.
    games: Vec<Game>,
    /// What order the user asked for the column in, which is what the settings
    /// file remembered from last time until they ask for another.
    ///
    /// Held beside the library rather than applied to it, so that the order
    /// somebody chose and the order Steam sent stay two separate things: the
    /// library is replaced wholesale every time a download finishes, and a list
    /// re-sorted in place would have to be re-sorted again on arrival — or
    /// would compare unequal to the one that just came in and rebuild the
    /// column for no reason. See the equality check in [`Steam::apply`].
    sort: lxb_steam::library::Sort,
    /// The sign-in on screen, if one is.
    signing_in: Option<Stage>,
    /// The games being fetched, and how far each has got. Kept here rather
    /// than on the game rows because the library is replaced wholesale
    /// whenever it is refreshed, and a download outlives several of those.
    fetching: BTreeMap<u32, Fetching>,
    /// And the ones being taken off the disk, for the same reason. Removing a
    /// game is quick but it is not instant, and a row that said nothing while
    /// it happened would be a row somebody pressed again.
    removing: BTreeSet<u32>,
    /// Whether this session drives Valve's client itself.
    ///
    /// False in a session started with `--no-steam` and in one showing an
    /// invented library, which is exactly the difference that decides whether
    /// the client's windows are the shell's to hide: a session that never
    /// starts the client has no business hiding one somebody started for
    /// themselves. See [`Steam::driving`].
    driving: bool,
}

/// How far one download has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetching {
    pub done: u64,
    /// Zero until the manifests have been read, which is the short stretch at
    /// the start where the size is not yet known.
    pub total: u64,
}

impl Fetching {
    /// What the row under the game's name says while it is coming down.
    pub fn said(&self) -> String {
        match self.total {
            0 => "Installing…".to_string(),
            total => format!(
                "Installing… {:.0}%",
                (self.done as f64 / total as f64) * 100.0
            ),
        }
    }
}

/// What the sign-in panel is asking for.
#[derive(Debug)]
pub enum Stage {
    /// Which way to sign in. The first question, and the only one this shell
    /// asks rather than Steam.
    Choosing,
    /// A code on the screen, waiting to be photographed. `None` until the
    /// first one arrives, which is one round trip after the panel goes up.
    Qr(Option<lxb_steam::qr::Code>),
    /// The account name, being typed.
    Account(String),
    /// The password for it. See the module docs for where this goes.
    Password { account: String, secret: Secret },
    /// A Steam Guard code, being typed, and what Steam asked for.
    Code {
        confirmation: Confirmation,
        typed: String,
    },
    /// Something is happening elsewhere — Steam is being asked, or a phone is
    /// waiting to be pressed — and there is nothing to type.
    Waiting(String),
    /// It did not work, and this is what to tell the user.
    Failed(String),
    /// Authentication worked, but Steam did not deliver a usable catalogue.
    /// This is not a sign-out and retrying must not ask for credentials again.
    LibraryUnavailable(String),
}

/// What one pass over the worker's events changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changed {
    /// The library is different, so the column has to be rebuilt.
    pub library: bool,
    /// What the panel should say is different, so it has to be redrawn — or
    /// raised, or taken away.
    pub panel: bool,
    /// The Games row is different: somebody signed in or out.
    pub account: bool,
    /// Games that finished moving on or off the disk, however they finished. A
    /// list rather than one, because several can end in a single pass and none
    /// of them may be dropped.
    pub installed: Vec<Ended>,
    /// What Valve's background client has to say for itself, if it said
    /// anything. A press waiting to start a game is waiting on this.
    pub client: Option<lxb_steam::ClientReport>,
}

/// How one game's journey on or off the disk ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The game is on the disk and can be started.
    Done { app_id: u32 },
    /// It is not, and this is what to say. Nothing was left behind, so
    /// pressing again starts a whole download rather than resuming one.
    ///
    /// A download somebody stopped is not one of these. It is not a failure,
    /// there is nothing to say about it, and the row saying so is the answer.
    Failed { app_id: u32, why: Stopped },
    /// The game has gone from the disk, which is what was asked for. Nothing
    /// to announce: the row saying "Not installed" is the whole answer.
    Removed { app_id: u32 },
    /// It has not gone, and this is what to say. Worth interrupting somebody
    /// for, because they asked for space back and have not got it.
    RemoveFailed { app_id: u32, why: String },
}

/// What a keystroke did to the panel.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed {
    /// No field of this panel wanted it.
    Elsewhere,
    /// It went into the field, which now says something different.
    Into,
    /// The field is finished with — Return, or Escape.
    Done { submitted: bool },
}

impl Steam {
    pub fn start() -> Steam {
        // Before the worker can start anything: Valve's client is an
        // application this shell starts, and it gets what the others get. It
        // was the one that did not, and the whole of the difference was the
        // guide button — see [`crate::model::hide_guarded_pads_from_hidapi`]
        // and [`crate::pad_guard`].
        lxb_steam::client::confine_children_with(crate::model::hide_guarded_pads_from_hidapi);
        Steam {
            client: lxb_steam::Steam::start(),
            account: None,
            games: Vec::new(),
            // Whatever the file said, which is nothing on a machine where
            // nobody has chosen. Read here rather than when the first library
            // arrives, because the order is a preference and not a property of
            // any particular library: it holds across a sign-out, and the
            // settings are loaded before this is built.
            sort: crate::settings::steam_sort().unwrap_or_default(),
            signing_in: None,
            fetching: BTreeMap::new(),
            removing: BTreeSet::new(),
            driving: true,
        }
    }

    /// One that will never do anything, for a session that has turned Steam
    /// off and for tests.
    pub fn settled() -> Steam {
        Steam {
            client: lxb_steam::Steam::settled(),
            account: None,
            games: Vec::new(),
            sort: crate::settings::steam_sort().unwrap_or_default(),
            signing_in: None,
            fetching: BTreeMap::new(),
            removing: BTreeSet::new(),
            driving: false,
        }
    }

    /// Whether this session drives Valve's client itself, and so whether the
    /// client's windows are the shell's to keep off the screen.
    pub fn driving(&self) -> bool {
        self.driving
    }

    /// Pretend an account is signed in and owns these games.
    ///
    /// For `--debug-steam-library`, and needed for the reason the display
    /// fixtures are: the Steam column is built out of somebody's library, so
    /// there is no way to look at it — or to screenshot it, or to check that
    /// installed titles really do come first — without an account, a password
    /// and a network. Nothing is asked of Steam, nothing is signed in, and
    /// none of these rows can be started: the account name says as much on the
    /// row it appears on.
    pub fn invent(&mut self, games: &[(String, bool)]) {
        tracing::warn!(
            games = games.len(),
            "--debug-steam-library: these rows are invented and nothing in them will start"
        );
        self.account = Some("a made-up account".to_string());
        self.games = lxb_steam::library::sorted(
            games
                .iter()
                .enumerate()
                .map(|(at, (name, installed))| {
                    lxb_steam::Game::invented(at as u32 + 1, name.clone(), *installed)
                })
                .collect(),
        );
    }

    /// Who is signed in, if anybody.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    /// What order the column is listed in.
    pub fn sort(&self) -> lxb_steam::library::Sort {
        self.sort
    }

    /// List it in another one. Returns whether that is a change — choosing the
    /// order the column is already in is not news, and there is nothing to
    /// rebuild for it.
    pub fn set_sort(&mut self, sort: lxb_steam::library::Sort) -> bool {
        if self.sort == sort {
            return false;
        }
        self.sort = sort;
        true
    }

    /// What this library can be sorted by at all, which is what greys the rows
    /// of the Sort list that would do nothing. See [`lxb_steam::library::Orders`].
    pub fn orders(&self) -> lxb_steam::library::Orders {
        lxb_steam::library::orders(&self.games)
    }

    pub fn signed_in(&self) -> bool {
        self.account.is_some()
    }

    pub fn has_client(&self) -> bool {
        self.client.client_at().is_some()
    }

    /// Whether what is up is waiting to be typed into, which is what decides
    /// that keys are letters rather than buttons.
    pub fn field_wanted(&self) -> bool {
        matches!(
            self.signing_in,
            Some(Stage::Account(_) | Stage::Password { .. } | Stage::Code { .. })
        )
    }

    /// Whether what is being typed is a password, which is what decides that
    /// the board must not be taken away by Back — a password field with no
    /// keyboard on a console is a field that cannot be filled in.
    pub fn password_wanted(&self) -> bool {
        matches!(self.signing_in, Some(Stage::Password { .. }))
    }

    /// Take in everything the worker has said. Returns what has to be redrawn.
    pub fn sync(&mut self) -> Changed {
        let mut changed = Changed::default();
        for event in self.client.take() {
            let one = self.apply(event);
            changed.library |= one.library;
            changed.panel |= one.panel;
            changed.account |= one.account;
            changed.installed.extend(one.installed);
            // The latest of these is the one that matters: a pass carrying
            // both "waking" and "ready" is a client that is ready.
            changed.client = one.client.or(changed.client);
        }
        changed
    }

    /// What one thing the worker said does to the shell's own state.
    ///
    /// Split out of [`Self::sync`] rather than written inside its loop so that
    /// it can be exercised without a worker on the other end of the channel:
    /// what these events do to the panel is most of the behaviour in this
    /// module, and a test that had to start a thread and reach Steam to see
    /// any of it would test neither.
    fn apply(&mut self, event: Event) -> Changed {
        let mut changed = Changed::default();
        match event {
            Event::Installing {
                app_id,
                done,
                total,
            } => {
                let now = Fetching { done, total };
                changed.library |= self.fetching.insert(app_id, now) != Some(now);
            }
            Event::Installed { app_id, .. } => {
                self.fetching.remove(&app_id);
                // The row has to become a game that can be started, and what
                // decides that is the manifest on the disk rather than
                // anything said here. So: look again, now, rather than at the
                // next interval — somebody is watching this one finish.
                self.client.refresh();
                changed.library = true;
                changed.installed.push(Ended::Done { app_id });
            }
            Event::InstallFailed { app_id, why } => {
                self.fetching.remove(&app_id);
                changed.library = true;
                changed.installed.push(Ended::Failed { app_id, why });
            }
            Event::Uninstalling { app_id } => {
                changed.library |= self.removing.insert(app_id);
            }
            Event::Uninstalled { app_id } => {
                self.removing.remove(&app_id);
                // The same reason the finished download asks: what the row
                // says next is on the disk, and the disk has just changed.
                self.client.refresh();
                changed.library = true;
                changed.installed.push(Ended::Removed { app_id });
            }
            Event::UninstallFailed { app_id, why } => {
                self.removing.remove(&app_id);
                changed.library = true;
                changed.installed.push(Ended::RemoveFailed { app_id, why });
            }
            // Nothing to announce and nothing to explain: somebody asked for
            // this and the machine is as they left it. The row stops counting
            // and goes back to being a game that is not installed, which is
            // the whole of what happened.
            Event::InstallStopped { app_id } => {
                self.fetching.remove(&app_id);
                changed.library = true;
            }
            Event::Client(report) => {
                tracing::info!(?report, "Valve's background client");
                changed.client = Some(report);
            }
            Event::SignedIn(account) => {
                let name = Some(account.name);
                changed.account |= self.account != name;
                self.account = name;
                // The panel goes away the moment it has succeeded: what it
                // was asking has been answered, and the answer is a whole
                // column further along the bar.
                changed.panel |= self.signing_in.take().is_some();
            }
            Event::SignedOut => {
                changed.account |= self.account.take().is_some();
                changed.library |= !self.games.is_empty();
                self.games.clear();
                // Only a sign-in that was under way: a session that starts
                // with nobody signed in says so, and there is no panel up
                // for it to be about.
                changed.panel |= self.signing_in.take().is_some();
            }
            Event::Library(games) => {
                changed.library |= self.games != games;
                self.games = games;
                if matches!(self.signing_in, Some(Stage::LibraryUnavailable(_))) {
                    self.signing_in = None;
                    changed.panel = true;
                }
            }
            Event::Challenge { code, .. } => {
                self.signing_in = Some(Stage::Qr(Some(code)));
                changed.panel = true;
            }
            Event::CodeWanted(confirmation) => {
                self.signing_in = Some(Stage::Code {
                    confirmation,
                    typed: String::new(),
                });
                changed.panel = true;
            }
            Event::Waiting(note) => {
                self.signing_in = Some(Stage::Waiting(note));
                changed.panel = true;
            }
            Event::SignInFailed(why) => {
                // Only while a sign-in is on screen. A token refused in
                // the background is a sign-out, which arrives as one; a
                // panel conjured over the bar to report it would be the
                // shell interrupting somebody who was doing something
                // else.
                if self.signing_in.is_some() {
                    self.signing_in = Some(Stage::Failed(why));
                    changed.panel = true;
                }
            }
            Event::LibraryUnavailable(why) => {
                self.signing_in = Some(Stage::LibraryUnavailable(why));
                changed.panel = true;
            }
        }
        changed
    }

    /// The rows the Steam column is made of, in the order they go in.
    ///
    /// Empty whenever there is no column to be had — nobody signed in, or a
    /// library with nothing in it — which is what [`crate::apps::shelve_steam`]
    /// reads as "take the column away".
    ///
    /// This is where the order the user chose is put on: the library arrives
    /// installed-first and is kept that way, and every other order is a pass
    /// over borrowed rows on the way to the bar. Called when the library
    /// changes and when the order does, and not otherwise — nothing here is
    /// asked per frame.
    pub fn rows(&self) -> Vec<crate::apps::Entry> {
        if !self.signed_in() {
            return Vec::new();
        }
        let mut listing: Vec<&Game> = self.games.iter().collect();
        // The default order is the one the library is already in, so the
        // ordinary column costs nothing to build.
        if self.sort != lxb_steam::library::Sort::default() {
            listing.sort_by(|a, b| self.sort.compare(a, b));
        }
        listing
            .into_iter()
            .map(|game| {
                // A game this shell is moving on or off the disk says so, over
                // whatever the disk says: there is a stretch at the start of
                // each — before Valve's client has written anything — where
                // the disk still holds the answer to the previous question,
                // and a row that gave it would read as a press that did
                // nothing.
                let fetching = self.fetching.get(&game.app_id);
                let removing = self.removing.contains(&game.app_id);
                let note = match (removing, fetching) {
                    (true, _) => "Removing…".to_string(),
                    (false, Some(so_far)) => so_far.said(),
                    (false, None) => game.note(),
                };
                crate::apps::Entry::Game(crate::apps::Game {
                    app_id: game.app_id,
                    name: game.name.clone(),
                    note,
                    // Neither a game on its way in nor one on its way out can
                    // be started, and both are busy: one row state for the two
                    // of them, because what the bar does about it is the same.
                    installed: game.installed && !game.updating && !removing,
                    updating: game.updating || removing || fetching.is_some(),
                    steam_client: self.client.client_at().is_some(),
                })
            })
            .collect()
    }

    /// Ask the client to do one thing to one title — check it, remove it, or
    /// come to the front. Playing is not one of these; see [`Self::play`].
    pub fn tell(&self, app_id: u32, doing: Doing) -> Result<(), String> {
        self.client.tell(app_id, doing)
    }

    // --- the sign-in ------------------------------------------------------

    /// Raise the panel, on the first question.
    pub fn begin(&mut self) {
        self.signing_in = Some(Stage::Choosing);
    }

    /// Give up on whatever is on screen.
    pub fn cancel(&mut self) {
        let cancelling_sign_in = self
            .signing_in
            .as_ref()
            .is_some_and(|stage| !matches!(stage, Stage::LibraryUnavailable(_)));
        self.signing_in = None;
        if cancelling_sign_in {
            self.client.cancel_sign_in();
        }
    }

    pub fn sign_out(&mut self) {
        self.client.sign_out();
    }

    pub fn refresh(&mut self) {
        self.client.refresh();
    }

    /// Have Valve's client running and signed in, because a game is about to
    /// need it.
    ///
    /// Answered through [`Self::sync`] as [`Changed::client`]. On a client
    /// that is already up this is a few microseconds and one event; on a cold
    /// one it is most of a minute, which is what the loading screen is for.
    pub fn wake_client(&mut self) {
        self.client.wake_client();
    }

    /// Ask the running client to start one game.
    ///
    /// `rungameid` rather than `-applaunch`, because it is the one Steam
    /// registers for the whole of its own library — a title, a non-Steam
    /// shortcut, a tool — and it is what Steam's own shortcuts use.
    ///
    /// Nothing comes back. What says the game started is the game's own window
    /// arriving on the display, which is what the splash is already watching
    /// for; there is no answer from Steam to wait on and none to be had.
    ///
    /// Straight from this thread, unlike every other request to the client:
    /// this one is only ever reached with the client already up and signed in —
    /// the splash has just spent as long as it took waiting for exactly that —
    /// so what runs here is a courier that exits in milliseconds, and its
    /// answer is what decides whether the splash carries on or the press is
    /// refused. [`lxb_steam::client::open`] is still what runs it, so a client
    /// that has died in the meantime is started rather than waited on.
    pub fn play(&mut self, app_id: u32) -> Result<(), String> {
        let Some(where_it_is) = lxb_steam::client::Where::find() else {
            return Err("There is no Steam client installed on this machine.".to_string());
        };
        let options = lxb_steam::client::Options::found();
        lxb_steam::client::open(
            &where_it_is,
            options.as_ref(),
            &format!("steam://rungameid/{app_id}"),
        )
        .map_err(|error| format!("Steam would not start this game: {error}"))
    }

    /// Have Valve's client fetch a game the account owns and has not got.
    pub fn install(&mut self, app_id: u32) {
        self.client.install(app_id);
    }

    pub fn stop_installing(&mut self, app_id: u32) {
        self.client.stop_installing(app_id);
    }

    /// Take one game off the disk.
    ///
    /// Nothing of Steam's appears for this. Whoever calls it has already asked
    /// the user, because this is the point past which the game is gone — see
    /// the Uninstall row of the game menu.
    pub fn uninstall(&mut self, app_id: u32) {
        self.client.uninstall(app_id);
    }

    /// How far one game's download has got, if it is being fetched.
    pub fn fetching(&self, app_id: u32) -> Option<Fetching> {
        self.fetching.get(&app_id).copied()
    }

    pub fn is_fetching(&self, app_id: u32) -> bool {
        self.fetching.contains_key(&app_id)
    }

    /// Whether one game is on its way off the disk right now.
    pub fn is_removing(&self, app_id: u32) -> bool {
        self.removing.contains(&app_id)
    }

    /// One game out of the library, by its app id.
    pub fn game(&self, app_id: u32) -> Option<&Game> {
        self.games.iter().find(|game| game.app_id == app_id)
    }

    /// Where Steam publishes that game's pictures, as the library was told.
    ///
    /// The library is the only thing that knows: the paths arrive in the same
    /// record as the game's name, and [`crate::art`] has no catalogue to look
    /// them up in. Nothing for a game the account does not own — one on the disk
    /// from somebody else's library — which is asked for by name instead.
    pub fn pictures(&self, app_id: u32) -> Option<&lxb_steam::art::Published> {
        self.game(app_id).map(|game| &game.pictures)
    }

    /// Sign in by photographing a code.
    pub fn with_qr(&mut self) {
        self.signing_in = Some(Stage::Qr(None));
        self.client.sign_in_with_qr();
    }

    /// Sign in with an account name and a password, starting with the name.
    pub fn with_password(&mut self) {
        self.signing_in = Some(Stage::Account(String::new()));
    }

    /// Hand over whatever the panel is asking for.
    ///
    /// One command for every field of this panel rather than one each, which
    /// is a departure from how the two other password fields in this shell are
    /// answered — those are deliberately separate commands, because one hands
    /// its password to `sudo` and the other to PAM, and a single name for both
    /// would be one mis-routed press away from answering the wrong question.
    /// Here there is one destination: the sign-in that raised the panel. The
    /// stage that is up *is* which question is being answered, and there is no
    /// second place for an answer to go.
    pub fn submit(&mut self) {
        let Some(stage) = self.signing_in.take() else {
            return;
        };
        match stage {
            Stage::Account(account) if !account.trim().is_empty() => {
                self.signing_in = Some(Stage::Password {
                    account: account.trim().to_string(),
                    secret: Secret::default(),
                });
            }
            // An empty account name is not an answer, so the panel stays where
            // it is rather than sending Steam a sign-in for nobody.
            Stage::Account(account) => self.signing_in = Some(Stage::Account(account)),
            Stage::Password { account, secret } => {
                let mut password = lxb_steam::Password::default();
                // The one way out of a `Secret`, and the only copy: what the
                // sink holds is overwritten when the client has encrypted it.
                if secret.hand_to(&mut password).is_err() {
                    self.signing_in = Some(Stage::Failed(
                        "That password could not be handed over.".to_string(),
                    ));
                    return;
                }
                self.signing_in = Some(Stage::Waiting("Signing in.".to_string()));
                self.client.sign_in_with_password(account, password);
            }
            Stage::Code {
                confirmation,
                typed,
            } if !typed.trim().is_empty() => {
                self.client.submit_code(typed.trim().to_string());
                self.signing_in = Some(Stage::Waiting(confirmation.asked()));
            }
            other => self.signing_in = Some(other),
        }
    }

    /// Apply one keystroke to whichever field this panel has up.
    pub fn type_into(&mut self, stroke: crate::keyboard::Stroke) -> Typed {
        let Some(stage) = self.signing_in.as_mut() else {
            return Typed::Elsewhere;
        };
        // A field of one line has no use for Tab, the arrows or the function
        // keys, and letting them past to the bar underneath would move the
        // cursor off the row the panel came out of.
        let into = |text: &mut String| match stroke {
            crate::keyboard::Stroke::Char(character) => {
                text.push(character);
                Typed::Into
            }
            crate::keyboard::Stroke::BACKSPACE => {
                text.pop();
                Typed::Into
            }
            crate::keyboard::Stroke::ENTER => Typed::Done { submitted: true },
            crate::keyboard::Stroke::ESCAPE => Typed::Done { submitted: false },
            _ => Typed::Into,
        };

        match stage {
            Stage::Account(text) => into(text),
            Stage::Code { typed, .. } => into(typed),
            Stage::Password { secret, .. } => match stroke {
                crate::keyboard::Stroke::Char(character) => {
                    secret.push(character);
                    Typed::Into
                }
                crate::keyboard::Stroke::BACKSPACE => {
                    secret.pop();
                    Typed::Into
                }
                crate::keyboard::Stroke::ENTER => Typed::Done { submitted: true },
                crate::keyboard::Stroke::ESCAPE => Typed::Done { submitted: false },
                _ => Typed::Into,
            },
            _ => Typed::Elsewhere,
        }
    }

    /// What the panel says and offers, or `None` when there is no sign-in on
    /// screen.
    ///
    /// Built afresh from the stage every time rather than edited in place, for
    /// the reason the password panel is: what is on screen is a function of
    /// what is being asked and of a *count* of characters, so there is nothing
    /// on the panel to keep in step with anything.
    pub fn panel(&self) -> Option<Panel> {
        let stage = self.signing_in.as_ref()?;
        let heading = dialog::Line::Heading(
            if matches!(stage, Stage::LibraryUnavailable(_)) {
                "Steam library"
            } else {
                "Sign in to Steam"
            }
            .to_string(),
        );
        let cancel = menu::Entry::new(menu::Command::SteamCancel, "Cancel");

        let panel = match stage {
            Stage::Choosing => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(
                        "Your library appears on the bar as a column of its own.".to_string(),
                    ),
                    dialog::Line::Rule,
                ],
                buttons: vec![
                    menu::Entry::new(menu::Command::SteamWithQr, "Scan a code with your phone"),
                    menu::Entry::new(
                        menu::Command::SteamWithPassword,
                        "Type an account name and password",
                    ),
                    cancel.group(1),
                ],
                // On the code, which is the way in that needs no keyboard —
                // and this shell is driven with a thumb.
                start: 0,
                typing: false,
            },
            Stage::Qr(code) => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(match code {
                        Some(_) => "Scan this in the Steam app on your phone.".to_string(),
                        None => "Asking Steam for a code.".to_string(),
                    }),
                    match code {
                        Some(code) => dialog::Line::Qr(code.clone()),
                        // The panel keeps the room the code will take, so it
                        // does not grow under the user's eyes a moment after
                        // it opened.
                        None => dialog::Line::Waiting,
                    },
                    dialog::Line::Rule,
                ],
                buttons: vec![cancel],
                start: 0,
                typing: false,
            },
            Stage::Account(typed) => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note("Enter your Steam account name.".to_string()),
                    dialog::Line::Entry(typed.clone()),
                    dialog::Line::Rule,
                ],
                buttons: vec![menu::Entry::new(menu::Command::SteamSubmit, "Next"), cancel],
                start: 0,
                typing: true,
            },
            Stage::Password { account, secret } => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(format!("Enter the password for {account}.")),
                    dialog::Line::Secret {
                        typed: secret.typed(),
                    },
                    dialog::Line::Rule,
                ],
                buttons: vec![
                    menu::Entry::new(menu::Command::SteamSubmit, "Sign in"),
                    cancel,
                ],
                start: 0,
                typing: true,
            },
            Stage::Code {
                confirmation,
                typed,
            } => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(confirmation.asked()),
                    dialog::Line::Entry(typed.clone()),
                    dialog::Line::Rule,
                ],
                buttons: vec![
                    menu::Entry::new(menu::Command::SteamSubmit, "Confirm"),
                    cancel,
                ],
                start: 0,
                typing: true,
            },
            Stage::Waiting(note) => Panel {
                lines: vec![
                    heading,
                    dialog::Line::Note(note.clone()),
                    dialog::Line::Waiting,
                    dialog::Line::Rule,
                ],
                buttons: vec![cancel],
                start: 0,
                typing: false,
            },
            Stage::Failed(why) => Panel {
                lines: vec![heading, dialog::Line::Note(why.clone()), dialog::Line::Rule],
                buttons: vec![
                    menu::Entry::new(menu::Command::SteamSignIn, "Try again"),
                    cancel,
                ],
                // On trying again: the panel is only ever here because
                // somebody was in the middle of signing in.
                start: 0,
                typing: false,
            },
            Stage::LibraryUnavailable(why) => Panel {
                lines: vec![heading, dialog::Line::Note(why.clone()), dialog::Line::Rule],
                buttons: vec![
                    menu::Entry::new(menu::Command::SteamRefresh, "Try again"),
                    menu::Entry::new(menu::Command::SteamCancel, "Close"),
                ],
                start: 0,
                typing: false,
            },
        };
        Some(panel)
    }
}

/// One frame of the sign-in panel.
pub struct Panel {
    pub lines: Vec<dialog::Line>,
    pub buttons: Vec<menu::Entry>,
    pub start: usize,
    /// Whether the on-screen keyboard belongs over it.
    pub typing: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::Stroke;
    use std::path::PathBuf;

    fn typing(steam: &mut Steam, text: &str) {
        for character in text.chars() {
            assert_eq!(steam.type_into(Stroke::Char(character)), Typed::Into);
        }
    }

    /// The sign-in by account name is two questions, and the second one is
    /// about the name given to the first.
    #[test]
    fn the_password_panel_names_the_account_it_is_for() {
        let mut steam = Steam::settled();
        steam.begin();
        assert!(matches!(steam.signing_in, Some(Stage::Choosing)));

        steam.with_password();
        typing(&mut steam, "  someone  ");
        steam.submit();

        let Some(Stage::Password { account, .. }) = steam.signing_in.as_ref() else {
            panic!("the panel did not move on to the password");
        };
        assert_eq!(account, "someone", "the field was not trimmed");

        let panel = steam.panel().expect("a panel is up");
        assert!(panel.typing, "a field with no keyboard cannot be filled in");
        assert!(panel
            .lines
            .iter()
            .any(|line| matches!(line, dialog::Line::Secret { typed: 0 })));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| matches!(line, dialog::Line::Note(note) if note.contains("someone"))),
            "the panel does not say whose password it wants"
        );
    }

    /// An empty account name is not an answer: the panel stays where it is
    /// rather than asking Steam to sign in as nobody.
    #[test]
    fn an_empty_account_name_does_not_move_on() {
        let mut steam = Steam::settled();
        steam.with_password();
        typing(&mut steam, "   ");
        steam.submit();
        assert!(
            matches!(steam.signing_in, Some(Stage::Account(_))),
            "an empty name was accepted"
        );
    }

    /// The password is drawn from a count and never from itself, so nothing
    /// the panel holds is the password.
    #[test]
    fn the_panel_holds_a_count_and_never_the_password() {
        let mut steam = Steam::settled();
        steam.with_password();
        typing(&mut steam, "someone");
        steam.submit();
        typing(&mut steam, "hunter2");

        let panel = steam.panel().expect("a panel is up");
        assert!(panel
            .lines
            .iter()
            .any(|line| matches!(line, dialog::Line::Secret { typed: 7 })));
        let said = format!("{:?}", panel.lines);
        assert!(!said.contains("hunter2"), "{said}");
    }

    /// Every stage that has a field takes the keyboard, and none of the others
    /// does — a panel that swallowed keys with nothing to type into would take
    /// the bar's own keys with it.
    #[test]
    fn only_the_stages_with_a_field_want_the_keyboard() {
        let mut steam = Steam::settled();
        assert!(!steam.field_wanted(), "nothing is on screen");

        steam.begin();
        assert!(!steam.field_wanted());
        assert_eq!(steam.type_into(Stroke::Char('x')), Typed::Elsewhere);

        steam.with_qr();
        assert!(!steam.field_wanted());
        assert_eq!(steam.type_into(Stroke::Char('x')), Typed::Elsewhere);

        steam.with_password();
        assert!(steam.field_wanted());
        assert!(
            !steam.password_wanted(),
            "an account name is not a password"
        );
        typing(&mut steam, "someone");
        steam.submit();
        assert!(steam.password_wanted());

        // And when the panel has gone, the keyboard is the bar's again.
        steam.cancel();
        assert!(!steam.field_wanted());
        assert_eq!(steam.type_into(Stroke::Char('x')), Typed::Elsewhere);
    }

    /// Return finishes a field and Escape leaves it, and both say so rather
    /// than acting on their own — what happens next needs the whole shell.
    #[test]
    fn return_and_escape_finish_a_field() {
        let mut steam = Steam::settled();
        steam.with_password();
        assert_eq!(
            steam.type_into(Stroke::ENTER),
            Typed::Done { submitted: true }
        );
        assert_eq!(
            steam.type_into(Stroke::ESCAPE),
            Typed::Done { submitted: false }
        );
        // The arrows are the field's, so the bar underneath does not move
        // while somebody is typing into a panel over it.
        assert_eq!(steam.type_into(Stroke::Named("Left")), Typed::Into);
    }

    /// Only a failure that happened while a sign-in was on screen puts a panel
    /// up. A token refused in the background is a sign-out, and a panel
    /// conjured over the bar to report it would interrupt somebody who was
    /// doing something else entirely.
    #[test]
    fn a_failure_with_no_panel_up_does_not_conjure_one() {
        let mut steam = Steam::settled();
        steam.apply(Event::SignInFailed("no".to_string()));
        assert!(steam.panel().is_none());

        steam.begin();
        steam.apply(Event::SignInFailed("no".to_string()));
        assert!(matches!(steam.signing_in, Some(Stage::Failed(_))));
        let panel = steam.panel().expect("a panel is up");
        assert_eq!(panel.buttons.len(), 2, "there is a way on and a way out");
    }

    /// Signing in takes the panel away: what it was asking has been answered.
    #[test]
    fn success_takes_the_panel_away() {
        let mut steam = Steam::settled();
        steam.with_qr();
        let changed = steam.apply(Event::SignedIn(lxb_steam::Account {
            name: "someone".to_string(),
            steam_id: 1,
        }));
        assert!(changed.panel && changed.account);
        assert!(steam.panel().is_none());
        assert_eq!(steam.account(), Some("someone"));
    }

    #[test]
    fn a_library_failure_keeps_the_account_and_a_success_closes_its_panel() {
        let mut steam = Steam::settled();
        steam.apply(Event::SignedIn(lxb_steam::Account {
            name: "someone".to_string(),
            steam_id: 1,
        }));

        let changed = steam.apply(Event::LibraryUnavailable("try later".to_string()));
        assert!(changed.panel);
        assert_eq!(steam.account(), Some("someone"));
        let panel = steam.panel().expect("the failure is visible");
        assert_eq!(panel.buttons[0].command, menu::Command::SteamRefresh);

        let changed = steam.apply(Event::Library(Vec::new()));
        assert!(changed.panel, "the recovered catalogue closes the notice");
        assert!(steam.panel().is_none());
        assert_eq!(steam.account(), Some("someone"));
    }

    /// There is a stretch between the press and Valve's client writing
    /// anything to the disk — waking a cold client is most of a minute — and
    /// without the shell keeping this the row would read "Not installed" for
    /// the whole of it, which is a press that appears to have done nothing.
    #[test]
    fn a_game_that_is_coming_down_says_so_on_its_row() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), false)];

        let note = |steam: &Steam| match &steam.rows()[0] {
            crate::apps::Entry::Game(game) => (game.note.clone(), game.updating),
            _ => panic!("that is not a game row"),
        };
        assert_eq!(note(&steam), ("Not installed".to_string(), false));

        // Before the manifests have been read there is no total, and the row
        // still has to say something.
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 0,
            total: 0,
        });
        assert_eq!(note(&steam), ("Installing…".to_string(), true));

        let changed = steam.apply(Event::Installing {
            app_id: 504230,
            done: 300,
            total: 1000,
        });
        assert!(changed.library, "the row has to be drawn again");
        assert_eq!(note(&steam), ("Installing… 30%".to_string(), true));

        // The same number twice is not a redraw. A download sends a great many
        // of these and the bar is rebuilt for each one that changes anything.
        assert!(
            !steam
                .apply(Event::Installing {
                    app_id: 504230,
                    done: 300,
                    total: 1000,
                })
                .library
        );
    }

    #[test]
    fn a_download_that_ends_leaves_the_row_to_the_disk_again() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), false)];
        steam.apply(Event::Installing {
            app_id: 504230,
            done: 1,
            total: 2,
        });
        assert!(steam.is_fetching(504230));

        let done = steam.apply(Event::Installed {
            app_id: 504230,
            into: PathBuf::from("/games/Celeste"),
        });
        assert!(!steam.is_fetching(504230));
        assert_eq!(done.installed, vec![Ended::Done { app_id: 504230 }]);
        assert!(done.library);

        steam.apply(Event::Installing {
            app_id: 504230,
            done: 1,
            total: 2,
        });
        let failed = steam.apply(Event::InstallFailed {
            app_id: 504230,
            why: Stopped::Failed("the line went away".to_string()),
        });
        assert!(
            !steam.is_fetching(504230),
            "and the row goes back to the disk"
        );
        assert_eq!(
            failed.installed,
            vec![Ended::Failed {
                app_id: 504230,
                why: Stopped::Failed("the line went away".to_string())
            }]
        );
    }

    /// A game on its way off the disk says so too, and is not startable while
    /// it goes. Removing is quick but it is not instant, and the row has to
    /// answer for the press that started it — otherwise it reads as "Installed"
    /// right up until the game vanishes.
    #[test]
    fn a_game_being_removed_says_so_and_cannot_be_started() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), true)];

        let row = |steam: &Steam| match &steam.rows()[0] {
            crate::apps::Entry::Game(game) => (game.note.clone(), game.installed, game.updating),
            _ => panic!("that is not a game row"),
        };
        let (_, installed, _) = row(&steam);
        assert!(installed, "it starts out as a game that can be played");

        let changed = steam.apply(Event::Uninstalling { app_id: 504230 });
        assert!(changed.library, "the row has to be drawn again");
        assert!(steam.is_removing(504230));
        assert_eq!(row(&steam), ("Removing…".to_string(), false, true));

        // And when it has gone, the row goes back to being whatever the disk
        // says — which is what takes "Removing…" off it. Nothing else in the
        // session can, so an event that never arrived would strand it.
        let gone = steam.apply(Event::Uninstalled { app_id: 504230 });
        assert!(!steam.is_removing(504230));
        assert_eq!(gone.installed, vec![Ended::Removed { app_id: 504230 }]);
    }

    /// A removal that did not happen has to reach the user. They asked for the
    /// space back and have not got it, and the row alone cannot say so — it
    /// looks exactly like a game nobody touched.
    #[test]
    fn a_removal_that_failed_is_announced_and_lets_go_of_the_row() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), true)];

        steam.apply(Event::Uninstalling { app_id: 504230 });
        let failed = steam.apply(Event::UninstallFailed {
            app_id: 504230,
            why: "Steam would not take it".to_string(),
        });
        assert!(!steam.is_removing(504230), "the row stops saying Removing");
        assert_eq!(
            failed.installed,
            vec![Ended::RemoveFailed {
                app_id: 504230,
                why: "Steam would not take it".to_string()
            }]
        );
    }

    /// The two ways an install can end without the game arriving are not the
    /// same thing to say: one is that something went wrong, the other is that
    /// Steam has a question. The shell answers them with different panels, so
    /// they have to arrive as different values rather than as two wordings.
    #[test]
    fn a_game_that_wants_asking_about_is_not_a_failed_download() {
        let mut steam = Steam::settled();
        steam.account = Some("someone".to_string());
        steam.games = vec![Game::invented(504230, "Celeste".to_string(), false)];

        steam.apply(Event::Installing {
            app_id: 504230,
            done: 0,
            total: 0,
        });
        let asked = steam.apply(Event::InstallFailed {
            app_id: 504230,
            why: Stopped::Asks("an agreement to accept".to_string()),
        });
        assert!(
            !steam.is_fetching(504230),
            "the row stops counting either way"
        );
        assert_eq!(
            asked.installed,
            vec![Ended::Failed {
                app_id: 504230,
                why: Stopped::Asks("an agreement to accept".to_string())
            }]
        );
    }

    /// The rows the column is built from: none at all when nobody is signed
    /// in, which is what takes the column off the bar.
    #[test]
    fn a_signed_out_session_has_no_rows() {
        let mut steam = Steam::settled();
        steam.games = vec![];
        assert!(steam.rows().is_empty());
    }
}
