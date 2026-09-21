//! Who the account knows, and where each of them is.
//!
//! Steam pushes a friends list at every client that logs on, unasked, as a
//! plain list of numbers — `ClientFriendsList` is sixty account ids and a
//! relationship each, and nothing else. A number is not a row: what a person
//! is called, whether they are reachable, what they are playing and what
//! picture they use are all *persona state*, which Steam sends only for the
//! accounts a client says it wants it for. So the list arriving is the
//! beginning of the question rather than the answer, and this module is the
//! two halves of asking it:
//!
//! ```text
//!   logon ─► ClientFriendsList (ids) ─► ClientRequestFriendData (ids + own id)
//!                                                   │
//!                                                   ▼
//!                                          ClientPersonaState ──► a row
//! ```
//!
//! The second half then goes on arriving for the rest of the session, unasked,
//! every time one of those people signs in, starts a game or changes their
//! name. That is the whole reason the roster is *held* rather than fetched: a
//! shell that asked Steam for its friends list would be asking a question
//! Steam is already answering, and would still be a minute behind.
//!
//! ## Why the account itself is in the same list
//!
//! Everything above is true of the signed-in account too. The shell knows the
//! *account name* from the sign-in — that is what was typed, and it is not what
//! anybody is called on Steam. The nickname, the status and the picture are the
//! account's own persona state, and the only way to have them is to ask for
//! them the way a friend's are asked for: by putting the account's own id in
//! the same request. So [`Roster::me`] is the same [`Person`] as every row
//! below it, arriving by the same route.
//!
//! ## Why this session has to announce itself, and what it announces
//!
//! Steam does not push presence to a client that has not said it is there. A
//! session that logs on and sends no `ClientChangeStatus` is an *offline*
//! client: it is given its friends list, and it is given names and pictures out
//! of the persona cache, and every single one of those friends reads Offline
//! for ever. That is not a parse failure and there is nothing in the log about
//! it — the rows simply arrive complete and wrong, which is exactly how it was
//! first shipped. It is also what every other client that talks this protocol
//! does about it; the vendored Android client in `third_party/Pluvia` says so
//! in as many words at the one line where it announces itself.
//!
//! So this session announces, and **what it announces is what this machine's
//! Steam is already set to**: the state Valve's client keeps for the account in
//! its own config, read by [`crate::client::recorded_status`]. That is not a
//! preference of the shell's — it is the status a client started on this
//! machine would announce, because it is the one the client itself comes back
//! up wearing — and announcing anything else is how the panel and Valve's
//! window came to be showing two different answers to one question.
//!
//! It is sent with `persona_set_by_user` **false**, which is how a client says
//! "this is me coming up" rather than "the user has just chosen this". Which is
//! exactly what it is: this session is a second client on a machine whose
//! status has already been set, and it is reporting itself rather than voting.
//!
//! Where there is nothing to read — no client, or a Steam that has never been
//! opened here — it falls back to [`AS_QUIETLY_AS_IT_CAN`], **Invisible**: the
//! one state that cannot make somebody more visible than they chose to be, and
//! one that costs nothing at the other end, because Steam sends an invisible
//! client the whole of its friends' presence. That is what being invisible is
//! *for*.
//!
//! ## What the user may say instead
//!
//! Exactly one thing is written from this shell, and it is the account's own
//! status: [`CHOOSABLE`], sent by [`announce_ourselves`] with
//! `persona_set_by_user` **true**. That flag is the whole difference between
//! the two calls — one is a client reporting itself, the other is a person
//! choosing — and it is why they are one function rather than two.
//!
//! Nothing else writes. There is no way from here to add a friend, to send a
//! message or to change a name, and a row that offered one would be a further
//! thing to get wrong before the list itself is right.

use std::collections::{BTreeMap, BTreeSet};

use steam_cm_protocol::friends::PersonaState;

/// Which flags of persona state this session asks Steam to send.
///
/// `EClientPersonaStateFlag`, as Steam's own client spells it. Four of the five
/// are what a row is made of, and the fifth is the reason this constant is
/// written here rather than taken from the protocol crate's own — which asks
/// for status, name, last-seen and game and *not* for `Presence`, the flag the
/// avatar hash rides in on. Asking without it produces a roster where every
/// person is complete except that nobody has a picture, which reads as an
/// avatar fetch that is broken rather than as a question that was never asked.
///
/// | | |
/// |---|---|
/// | `Status` (1) | whether they are online, and how |
/// | `PlayerName` (2) | what they are called |
/// | `Presence` (16) | the avatar hash, among other things |
/// | `LastSeen` (64) | when they were last around |
/// | `GameExtraInfo` (256) | the name of the game they are in |
const WANTED_ABOUT_A_PERSON: u32 = 1 | 2 | 16 | 64 | 256;

/// `EFriendRelationship::Friend`. The list Steam pushes also carries people
/// who have been blocked and requests in both directions; only this one is a
/// friend.
const IS_A_FRIEND: u32 = 3;

/// What this session tells Steam it is, where there is nothing else to go on.
///
/// See the note at the head of this module: something has to be said or no
/// friend is ever reported as anything but offline. The first answer is the
/// status this machine's Steam is set to; this is the fallback for a machine
/// where there is no such thing to read, and it is the fallback because it is
/// the only thing that can be said without speaking for the user. Invisible is
/// signed in, is told everything, and is seen by nobody.
pub const AS_QUIETLY_AS_IT_CAN: Presence = Presence::Invisible;

/// The statuses a person may put this account into, in the order they are
/// offered.
///
/// Valve's own four, and they are Valve's own by measurement rather than by
/// taste: the running client's friends UI exports exactly `SetUserOnline`,
/// `SetUserAway`, `SetUserInvisible` and `SetUserOffline` and nothing else —
/// read off `g_FriendsUIApp.m_exportsCurrentUserStatus` on client build
/// 1785799196. A shell offering a fifth would be offering something the client
/// beside it cannot agree to.
///
/// The four that are missing are missing for the same reason. **Busy** and
/// **Snooze** are Steam's to decide — Snooze is what an idle client becomes on
/// its own — and **Looking to trade** and **Looking to play** are retired. All
/// four were tried through the client's own URL table and all four are silent
/// no-ops: `steam://friends/status/busy`, `.../snooze`, `.../trade` and
/// `.../play` were accepted and left the client's persona state exactly where
/// it was, while `online`, `away`, `invisible` and `offline` each moved it.
pub const CHOOSABLE: [Presence; 4] = [
    Presence::Online,
    Presence::Away,
    Presence::Invisible,
    Presence::Offline,
];

/// Where somebody is, in Steam's own terms.
///
/// Steam's `EPersonaState` verbatim rather than reduced to online/offline,
/// because the difference is the whole of what the row under a name says. Away
/// and Busy are things a person chose to tell their friends and a shell that
/// flattened them to "Online" would be dropping the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Offline,
    Online,
    Busy,
    Away,
    Snooze,
    LookingToTrade,
    LookingToPlay,
    /// Signed in and telling everybody else they are not.
    ///
    /// Only ever seen about the account this session is signed in as: a friend
    /// who is invisible arrives as [`Presence::Offline`], because that is what
    /// Steam tells everybody about them and choosing it is choosing that.
    Invisible,
}

impl Presence {
    fn of(state: PersonaState) -> Presence {
        match state {
            PersonaState::Offline => Presence::Offline,
            PersonaState::Online => Presence::Online,
            PersonaState::Busy => Presence::Busy,
            PersonaState::Away => Presence::Away,
            PersonaState::Snooze => Presence::Snooze,
            PersonaState::LookingToTrade => Presence::LookingToTrade,
            PersonaState::LookingToPlay => Presence::LookingToPlay,
            PersonaState::Invisible => Presence::Invisible,
        }
    }

    /// Back into Steam's own number, for the one message this shell sends
    /// about itself. The inverse of [`Presence::of`], and it has to stay that
    /// way: a status that was sent as one state and read back as another would
    /// be a button whose tick lands on a different row from the one pressed.
    fn to_state(self) -> PersonaState {
        match self {
            Presence::Offline => PersonaState::Offline,
            Presence::Online => PersonaState::Online,
            Presence::Busy => PersonaState::Busy,
            Presence::Away => PersonaState::Away,
            Presence::Snooze => PersonaState::Snooze,
            Presence::LookingToTrade => PersonaState::LookingToTrade,
            Presence::LookingToPlay => PersonaState::LookingToPlay,
            Presence::Invisible => PersonaState::Invisible,
        }
    }

    /// Whether Valve's client writes this status into its own config, so that
    /// a status handed to it can be read back.
    ///
    /// All of them but Offline. Measured on this machine on 2026-09-03:
    /// `steam://friends/status/` for online, away and invisible each moved the
    /// client's record within four seconds, and offline did not move it at all
    /// — going offline is the client leaving the friends network rather than a
    /// state it remembers being in. So an Offline can be sent to a client but
    /// never confirmed, and the shell hands one over once rather than going on
    /// asking for an agreement nothing will ever write down. See
    /// [`crate::client::recorded_status`].
    pub fn is_written_down(self) -> bool {
        !matches!(self, Presence::Offline)
    }

    /// Steam's own number for this status, for writing one down.
    ///
    /// The inverse of [`Presence::from_number`], and a number rather than the
    /// enum's own name because what is written down outlives the shape of a
    /// Rust type — and because the number is what Steam and Valve's client both
    /// already speak. See [`crate::session::Owed`].
    pub fn to_number(self) -> u32 {
        self.to_state().to_raw()
    }

    /// The status Steam's own number means, where the number came off the
    /// disk rather than off the wire.
    ///
    /// `None` for anything that is not one of the eight, and that is the point
    /// of it being an `Option`: this reads a field out of Valve's client's
    /// config — see [`crate::client::recorded_status`] — and the answer decides
    /// what this session announces about the account. A number nobody can read
    /// has to mean *nothing was read*, so the announcement falls back to
    /// [`AS_QUIETLY_AS_IT_CAN`]. [`PersonaState::from_raw`] would answer
    /// `Offline` for it, which is a config this shell could not parse deciding
    /// the user is offline.
    pub fn from_number(raw: u32) -> Option<Presence> {
        Some(match raw {
            0 => Presence::Offline,
            1 => Presence::Online,
            2 => Presence::Busy,
            3 => Presence::Away,
            4 => Presence::Snooze,
            5 => Presence::LookingToTrade,
            6 => Presence::LookingToPlay,
            7 => Presence::Invisible,
            _ => return None,
        })
    }

    /// What Valve's client calls this status in its own URL table, for the one
    /// message that has to reach the client rather than Steam.
    ///
    /// `None` for the four that are not [`CHOOSABLE`]: their verbs exist and do
    /// nothing, and a shell that sent one would be reporting success for a
    /// press that changed nothing.
    ///
    /// This is how the client is told, and it is a URL rather than a call into
    /// its JavaScript because a URL needs nothing of it. The interface half is
    /// only listening when the client was started with the debugging marker in
    /// place — see [`crate::webui`] — which the ordinary session deliberately
    /// does not do; a URL reaches a client somebody started for themselves.
    pub fn url_verb(self) -> Option<&'static str> {
        Some(match self {
            Presence::Online => "online",
            Presence::Away => "away",
            Presence::Invisible => "invisible",
            Presence::Offline => "offline",
            Presence::Busy | Presence::Snooze => return None,
            Presence::LookingToTrade | Presence::LookingToPlay => return None,
        })
    }

    /// Whether choosing this means leaving the friends network rather than
    /// standing somewhere in it.
    ///
    /// The one status that is not a state to be *seen* in: a session that has
    /// announced it is told nothing further about anybody, which is exactly
    /// what Steam's own Offline is. See [`Roll::went_offline`], which is what
    /// the CM session does about it.
    pub fn is_away_from_it_all(self) -> bool {
        matches!(self, Presence::Offline)
    }

    /// Whether Steam considers this person reachable.
    ///
    /// Invisible counts, and it counts for the one account it is ever seen
    /// about: somebody signed in as invisible *is* signed in, and their own
    /// shell saying Offline at them would be telling them something untrue
    /// about themselves.
    pub fn is_around(self) -> bool {
        !matches!(self, Presence::Offline)
    }

    /// What to write under the name when there is no game to write instead.
    pub fn said(self) -> &'static str {
        match self {
            Presence::Offline => "Offline",
            Presence::Online => "Online",
            Presence::Busy => "Busy",
            Presence::Away => "Away",
            Presence::Snooze => "Snooze",
            Presence::LookingToTrade => "Looking to trade",
            Presence::LookingToPlay => "Looking to play",
            Presence::Invisible => "Invisible",
        }
    }
}

/// Which of the three bands a row belongs to, which is the order the user
/// asked the list to be in: in a game first, reachable second, gone last.
///
/// Its own type rather than a number, because the headings above the bands are
/// drawn from it and a heading and a sort that could disagree would be two
/// answers to one question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    Playing,
    Around,
    Away,
}

impl Band {
    /// What the rule above the band says.
    pub fn said(self) -> &'static str {
        match self {
            Band::Playing => "In game",
            Band::Around => "Online",
            Band::Away => "Offline",
        }
    }
}

/// One person: the account signed in, or somebody they know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub steam_id: u64,
    /// What they call themselves on Steam — the nickname, never the account
    /// name. Empty for a person Steam has sent an id for and not yet a persona,
    /// which is an ordinary state for the first second of a session.
    pub name: String,
    pub presence: Presence,
    /// The game they are in, in Steam's own words. `None` for somebody who is
    /// not in one, and for one Steam declines to name — a private profile, or
    /// a game being played through a server this account cannot see.
    pub game: Option<String>,
    /// And its id, where Steam sent one. Carried for the sake of a picture
    /// later; nothing is drawn from it today.
    pub app_id: Option<u32>,
    /// The account's picture, as the hash Steam files it under — lowercase hex,
    /// which is exactly the form the avatar host wants in a path. `None` for an
    /// account with no picture of its own, which is drawn as the mark the row
    /// would have had anyway.
    pub avatar: Option<String>,
}

impl Person {
    /// Somebody Steam has named in a friends list and said nothing else about
    /// yet.
    ///
    /// A row rather than a gap, because the list arrives whole and the personas
    /// arrive in batches over the second after it: rows that appeared one at a
    /// time as their names landed would be a list that reshuffles itself while
    /// somebody is reading it.
    fn unnamed(steam_id: u64) -> Person {
        Person {
            steam_id,
            name: String::new(),
            presence: Presence::Offline,
            game: None,
            app_id: None,
            avatar: None,
        }
    }

    /// Which band this row goes in.
    ///
    /// Playing beats reachable even when the game is unnamed: what puts
    /// somebody in the first band is that they are *in* something, and a game
    /// Steam will not name is still a game.
    pub fn band(&self) -> Band {
        if self.app_id.is_some() || self.game.is_some() {
            Band::Playing
        } else if self.presence.is_around() {
            Band::Around
        } else {
            Band::Away
        }
    }

    /// What the line under the name says: the game, or where they are.
    ///
    /// Somebody in a game Steam has not named yet is *in a game*, which is what
    /// the row says — never "Online", which is true of them and is not what
    /// they are doing. See [`Roll::games_to_name`]: the name arrives a moment
    /// later and replaces this, and for a game Steam will not name it never
    /// arrives at all.
    pub fn doing(&self) -> &str {
        match (&self.game, self.app_id) {
            (Some(game), _) => game,
            (None, Some(_)) => "In game",
            (None, None) => self.presence.said(),
        }
    }

    /// Where this account's picture is published, at the size wanted.
    ///
    /// `None` for an account with no picture. The three sizes are Valve's own
    /// and are the only ones the host serves.
    pub fn avatar_url(&self, size: AvatarSize) -> Option<String> {
        Some(format!(
            "{AVATARS}/{}{}.jpg",
            self.avatar.as_ref()?,
            size.suffix()
        ))
    }
}

/// Where Steam publishes the pictures accounts are recognised by.
///
/// Public and needs no account: it is the host every profile page on the web
/// pulls its avatars from.
const AVATARS: &str = "https://avatars.steamstatic.com";

/// Which of the three sizes Steam keeps an avatar at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvatarSize {
    /// 32 px. Too small for anything this shell draws; named for completeness
    /// so a caller reaching for it finds the reason rather than the URL.
    Small,
    /// 64 px, which is a row of a list.
    Medium,
    /// 184 px, which is the picture at the head of the panel.
    Full,
}

impl AvatarSize {
    fn suffix(self) -> &'static str {
        match self {
            AvatarSize::Small => "",
            AvatarSize::Medium => "_medium",
            AvatarSize::Full => "_full",
        }
    }
}

/// The account, and everybody it knows, in the order they are drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster {
    /// The signed-in account's own persona. `None` until Steam has answered
    /// about it, which is the same second the first friends arrive.
    pub me: Option<Person>,
    /// Everybody else, already in band order — see [`Person::band`] — and by
    /// name within each band.
    pub friends: Vec<Person>,
}

impl Roster {
    /// How many rows are in each band, in band order, for the headings.
    pub fn counts(&self) -> [usize; 3] {
        let mut counts = [0; 3];
        for friend in &self.friends {
            counts[friend.band() as usize] += 1;
        }
        counts
    }
}

/// What the CM session keeps between one persona push and the next.
///
/// Steam sends the friends list once and then persona state forever, in
/// batches of whoever happened to change — so neither message on its own is a
/// roster, and the roster is what this accumulates. It is also why a persona
/// arriving for somebody who is not in the list is dropped rather than added:
/// Steam sends personas for people met in a chat room or a lobby as well as for
/// friends, and a friends list that grew a row every time somebody was seen in
/// a game would not be a friends list.
#[derive(Debug, Default)]
pub struct Roll {
    /// The signed-in account, so its own persona can be told from a friend's.
    me: u64,
    /// Everybody Steam has said is a friend, and the last thing it said about
    /// each. A `BTreeMap` so the order rows are *held* in is settled, which is
    /// what makes two rosters built from the same facts compare equal.
    people: BTreeMap<u64, Person>,
    /// And the account's own persona, which is not a friend and is not in the
    /// map above.
    mine: Option<Person>,
    /// What Steam calls the games friends are in, once it has been asked.
    ///
    /// `ClientPersonaState` carries `game_played_app_id` and very often no
    /// `game_name` at all — Valve's own client resolves the title out of the
    /// appinfo it already holds, and a shell that did not would draw "In game"
    /// beside every friend who is in one. So the id is looked up in PICS and
    /// the answer kept here, beside the roster rather than in it, because it is
    /// a fact about the game and the roster is replaced whenever anybody moves.
    games: BTreeMap<u32, String>,
    /// And the ids that have been asked about, named or not.
    ///
    /// A game Steam will not name must be asked about once and never again, or
    /// every persona push would start another round trip about the same id.
    asked_about: BTreeSet<u32>,
    /// Whether this session has told Steam it is offline.
    ///
    /// Steam's own Offline, and the whole of what it means here: the roster
    /// this publishes has nobody in it, and nobody but the account itself is
    /// asked about. Not a second copy of the friends list — [`Self::people`]
    /// keeps every id and everything last known about them — because coming
    /// back is a status somebody chooses a second later and a list that had
    /// been thrown away would have to be waited for again.
    ///
    /// Held here rather than read off [`Self::mine`] even though the two agree,
    /// because they agree only *after* Steam has answered: the session goes
    /// quiet the moment it is asked to, and a roster published in between must
    /// not be a full one.
    offline: bool,
}

impl Roll {
    /// A session that is taking part in the friends network.
    pub fn about(me: u64) -> Roll {
        Roll::about_and(me, false)
    }

    /// The same, for a session that comes up already offline: somebody chose
    /// Offline and the connection dropped, and the choice outlives the
    /// connection. See [`crate::cm::Chosen`].
    pub fn about_and(me: u64, offline: bool) -> Roll {
        Roll {
            me,
            people: BTreeMap::new(),
            mine: None,
            games: BTreeMap::new(),
            asked_about: BTreeSet::new(),
            offline,
        }
    }

    /// Everybody Steam has said is a friend, plus the account itself — the ids
    /// worth asking persona state about.
    ///
    /// What [`Self::listed`] answered when the list arrived, asked again: it is
    /// how a session coming back from Offline picks the whole roster up again
    /// without waiting for Steam to send a list it only sends once.
    pub fn everyone(&self) -> Vec<u64> {
        let mut asking: Vec<u64> = self.people.keys().copied().collect();
        asking.push(self.me);
        asking
    }

    /// Take part in the friends network, or stop. Answers whether that is a
    /// change, and so whether there is a fresh roster to publish.
    ///
    /// Going quiet does not forget anybody. See [`Self::offline`].
    pub fn went_offline(&mut self, offline: bool) -> bool {
        let was = self.offline;
        self.offline = offline;
        was != offline
    }

    /// Whether this session has gone quiet.
    pub fn is_offline(&self) -> bool {
        self.offline
    }

    /// Whether Steam still says this account is a friend.
    ///
    /// The check every conversation is made against, and it is asked of
    /// [`Self::people`] rather than of [`Self::roster`] deliberately: a session
    /// standing offline publishes an empty roster and has not stopped being
    /// anybody's friend. Somebody who was unfriended is gone from here the
    /// moment Steam's incremental list says so — see [`Self::listed`] — which
    /// is what makes this the last word before a message goes out.
    pub fn is_a_friend(&self, steam_id: u64) -> bool {
        self.people.contains_key(&steam_id)
    }

    /// Which games in the roster Steam has not been asked to name yet.
    ///
    /// Only the ones somebody is actually in, and only once each: this is a
    /// PICS round trip, and it is made from the packet loop.
    pub fn games_to_name(&self) -> Vec<u32> {
        let mut wanted: Vec<u32> = self
            .people
            .values()
            .filter(|person| person.game.is_none())
            .filter_map(|person| person.app_id)
            .filter(|app_id| !self.asked_about.contains(app_id))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        wanted
    }

    /// Mark these app ids as having a PICS lookup in flight.
    ///
    /// This happens before the asynchronous lookup is started. Persona pushes
    /// continue to be folded while it runs, and without this mark each one
    /// would start another lookup for the same game.
    pub fn began_naming(&mut self, app_ids: &[u32]) {
        self.asked_about.extend(app_ids.iter().copied());
    }

    /// Take what Steam called them. `asked` is every id that was looked up,
    /// including the ones it declined to name — those are marked so the
    /// question is not asked again.
    pub fn named(&mut self, asked: &[u32], names: BTreeMap<u32, String>) -> bool {
        self.asked_about.extend(asked.iter().copied());
        let moved = names
            .iter()
            .any(|(app_id, name)| self.games.get(app_id) != Some(name));
        self.games.extend(names);
        moved
    }

    /// Take Steam's friends list. Answers the ids worth asking about — every
    /// friend, and the account itself.
    ///
    /// An id already known keeps whatever persona it has: a list that arrives
    /// again mid-session must not blank every name on the screen.
    pub fn listed(
        &mut self,
        friends: &[steam_cm_protocol::friends::Friend],
        incremental: bool,
    ) -> Vec<u64> {
        let named: Vec<u64> = friends
            .iter()
            .filter(|friend| friend.relationship == IS_A_FRIEND)
            .map(|friend| friend.steamid)
            .collect();
        if incremental {
            // A patch names only the relationships which changed. Anything
            // other than Friend removes that one id and leaves every id not
            // mentioned exactly where it was.
            for friend in friends {
                if friend.relationship != IS_A_FRIEND {
                    self.people.remove(&friend.steamid);
                }
            }
        } else {
            // The logon packet is a complete snapshot, including an empty
            // account. Replace rather than union it with an older snapshot.
            let keep: BTreeSet<u64> = named.iter().copied().collect();
            self.people.retain(|id, _| keep.contains(id));
        }
        for id in &named {
            self.people
                .entry(*id)
                .or_insert_with(|| Person::unnamed(*id));
        }
        // Only the account itself while this session is offline. Steam sends
        // the list whether or not anybody is listening, and asking about
        // sixty people whose presence will not be sent is the traffic Offline
        // is *for* stopping — but the account's own persona is what puts a
        // name and a picture at the head of the panel, and that has to arrive
        // however quiet the rest of it is.
        let mut asking = match self.offline {
            true => Vec::new(),
            false => named,
        };
        asking.push(self.me);
        asking
    }

    /// Take a batch of persona state. `true` when anything on screen changed.
    pub fn heard(&mut self, personas: &[steam_cm_protocol::friends::Persona]) -> bool {
        let mut moved = false;
        for persona in personas {
            let known = if persona.steamid == self.me {
                self.mine.get_or_insert_with(|| Person::unnamed(self.me))
            } else {
                match self.people.get_mut(&persona.steamid) {
                    Some(known) => known,
                    // Somebody met in a lobby rather than a friend. See the
                    // note on [`Roll`].
                    None => continue,
                }
            };
            let before = known.clone();
            if !persona.name.is_empty() {
                known.name = persona.name.clone();
            }
            known.presence = Presence::of(persona.state.clone());
            // The game is replaced only when Steam sent the game fields at
            // all. A persona push carrying nothing but a new name would
            // otherwise take a friend out of the game they are still in — see
            // `game_fields_present`, which is the protocol crate's own answer
            // to exactly this.
            if persona.game_fields_present {
                known.app_id = persona.game_app_id;
                known.game = persona.game_name.clone();
            }
            if let Some(hash) = persona.avatar_hash.as_ref() {
                known.avatar = avatar_of(hash);
            }
            moved |= *known != before;
        }
        moved
    }

    /// Put down what this session has just told Steam about the account, so
    /// that the panel says it before Steam has said it back.
    ///
    /// Steam does echo a `ClientPersonaState` for the account itself after a
    /// `ClientChangeStatus`, and when it arrives it replaces this wholesale —
    /// which is the point: this is not a second source of truth, it is the same
    /// fact arriving a round trip early. Without it the one line under the
    /// nickname is the one thing on the panel that does not answer a press.
    ///
    /// Nothing at all before Steam has said who this account is: there is no
    /// row on the screen to correct, and inventing one here would draw a
    /// nameless, pictureless person at the head of the panel.
    pub fn told_steam(&mut self, presence: Presence) -> bool {
        let Some(mine) = self.mine.as_mut() else {
            return false;
        };
        let moved = mine.presence != presence;
        mine.presence = presence;
        moved
    }

    /// The roster as it stands, in the order it is drawn.
    ///
    /// Nobody but the account itself while this session is offline. That is
    /// Steam's own Offline rather than an emptiness invented here: a session
    /// that has announced it is told nothing further about anybody, so a list
    /// drawn from what was last known would be a column of names going stale
    /// on the screen. What is *kept* is the same list — see [`Self::offline`]
    /// — so coming back is a push away rather than a reconnect.
    /// What one person is playing, as the id and whatever it is called.
    ///
    /// For the one thing that needs a single person rather than the list: an
    /// invitation to a game names no game — see [`crate::chat::Invite`] — so
    /// the app is whatever the person doing the inviting is in at the moment
    /// they ask. Answered even for somebody standing offline on Steam, because
    /// an invitation from them is still an invitation; what `offline` hides is
    /// the *list*, and this is not it.
    pub fn playing(&self, steam_id: u64) -> (Option<u32>, Option<String>) {
        match self.people.get(&steam_id) {
            Some(person) => (
                person.app_id,
                person.game.clone().or_else(|| {
                    person
                        .app_id
                        .and_then(|app_id| self.games.get(&app_id).cloned())
                }),
            ),
            None => (None, None),
        }
    }

    pub fn roster(&self) -> Roster {
        if self.offline {
            return Roster {
                me: self.mine.clone(),
                friends: Vec::new(),
            };
        }
        let mut friends: Vec<Person> = self
            .people
            .values()
            .cloned()
            // The name Steam sent, where it sent one, and otherwise whatever
            // the lookup found for the id. Filled in here rather than written
            // into the person, so that a persona push carrying a real
            // `game_name` always wins over a resolved title and the two can
            // never drift.
            .map(|mut person| {
                if person.game.is_none() {
                    person.game = person
                        .app_id
                        .and_then(|app_id| self.games.get(&app_id))
                        .cloned();
                }
                person
            })
            .collect();
        // Band first, then by name as somebody reading the list would look for
        // it: case folded, because a list where `alice` sorts after `Zoe` is a
        // list nobody can find a name in. The id breaks a tie so that two
        // people who chose the same nickname keep a settled order rather than
        // swapping places on every push.
        friends.sort_by(|a, b| {
            a.band()
                .cmp(&b.band())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.steam_id.cmp(&b.steam_id))
        });
        Roster {
            me: self.mine.clone(),
            friends,
        }
    }
}

/// The avatar hash as the URL wants it, or `None` for an account that has none.
///
/// Steam sends twenty zero bytes for an account that has never set a picture,
/// which is not a hash and addresses nothing: asking the avatar host for it
/// gets a 404 for every such account in the list at once.
fn avatar_of(hash: &[u8]) -> Option<String> {
    if hash.iter().all(|byte| *byte == 0) {
        return None;
    }
    let mut hex = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    Some(hex)
}

/// Ask Steam to send persona state for these accounts, and to go on sending it.
///
/// Built here rather than through the protocol crate's own helper for the
/// reason [`WANTED_ABOUT_A_PERSON`] gives: its flags leave out the one the
/// picture arrives under.
pub fn ask_about(
    state: &steam_cm_protocol::connection::ConnectionState,
    people: Vec<u64>,
) -> (
    steam_cm_protocol::protobuf::CMsgProtoBufHeader,
    steam_cm_protocol::protobuf::CMsgClientRequestFriendData,
) {
    (
        steam_cm_protocol::protobuf::CMsgProtoBufHeader {
            steamid: state.steamid,
            client_sessionid: state.client_session_id,
            ..Default::default()
        },
        steam_cm_protocol::protobuf::CMsgClientRequestFriendData {
            persona_state_requested: Some(WANTED_ABOUT_A_PERSON),
            friends: people,
        },
    )
}

/// Tell Steam where this session stands, so that it starts sending presence.
///
/// The whole of what makes a friends list a friends list rather than a list of
/// names — see the head of this module.
///
/// Two callers, one message, and the difference between them is `chosen`. A
/// session coming up passes [`AS_QUIETLY_AS_IT_CAN`] and `false`, which is a
/// client reporting itself; somebody picking a status off the panel passes what
/// they picked and `true`, which is a preference. Steam is told which of the
/// two it is because the two mean different things at the other end, and
/// because a shell that sent every announcement as a choice would be putting a
/// background session's opinion on the account's record.
pub fn announce_ourselves(
    state: &steam_cm_protocol::connection::ConnectionState,
    presence: Presence,
    chosen: bool,
) -> (
    steam_cm_protocol::protobuf::CMsgProtoBufHeader,
    steam_cm_protocol::protobuf::CMsgClientChangeStatus,
) {
    (
        steam_cm_protocol::protobuf::CMsgProtoBufHeader {
            steamid: state.steamid,
            client_sessionid: state.client_session_id,
            ..Default::default()
        },
        steam_cm_protocol::protobuf::CMsgClientChangeStatus {
            persona_state: Some(presence.to_state().to_raw()),
            persona_set_by_user: Some(chosen),
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(
        id: u64,
        name: &str,
        state: PersonaState,
        game: Option<&str>,
    ) -> steam_cm_protocol::friends::Persona {
        steam_cm_protocol::friends::Persona {
            steamid: id,
            name: name.to_string(),
            state,
            game_app_id: game.map(|_| 220),
            game_name: game.map(str::to_string),
            avatar_hash: None,
            game_fields_present: game.is_some(),
        }
    }

    fn listed(ids: &[u64]) -> Vec<steam_cm_protocol::friends::Friend> {
        ids.iter()
            .map(|id| steam_cm_protocol::friends::Friend {
                steamid: *id,
                relationship: IS_A_FRIEND,
            })
            .collect()
    }

    /// What a session that nobody has spoken for says about itself, and the
    /// only state that cannot make somebody more visible than they chose to be.
    #[test]
    fn this_session_announces_itself_invisible_and_not_as_a_choice() {
        let state = a_connection();
        let (header, body) = announce_ourselves(&state, AS_QUIETLY_AS_IT_CAN, false);
        assert_eq!(header.steamid, Some(7));
        assert_eq!(header.client_sessionid, Some(3));
        assert_eq!(body.persona_state, Some(PersonaState::Invisible.to_raw()));
        assert_eq!(body.persona_set_by_user, Some(false));
        // Never Online. A background session that announced itself online would
        // be putting its own opinion above the user's.
        assert_ne!(body.persona_state, Some(PersonaState::Online.to_raw()));
    }

    /// And what it says when somebody has: the state they picked, marked as a
    /// choice. The flag is the whole difference between the two messages.
    #[test]
    fn a_status_somebody_picked_is_sent_as_a_choice() {
        let state = a_connection();
        for picked in CHOOSABLE {
            let (_, body) = announce_ourselves(&state, picked, true);
            assert_eq!(
                body.persona_state,
                Some(picked.to_state().to_raw()),
                "{picked:?} was not the state that was sent"
            );
            assert_eq!(body.persona_set_by_user, Some(true), "{picked:?}");
        }
    }

    /// Every state Steam can send about somebody survives the trip back out to
    /// Steam's own number, so a tick lands on the row that was pressed.
    #[test]
    fn a_state_read_and_sent_again_is_the_same_state() {
        for state in [
            PersonaState::Offline,
            PersonaState::Online,
            PersonaState::Busy,
            PersonaState::Away,
            PersonaState::Snooze,
            PersonaState::LookingToTrade,
            PersonaState::LookingToPlay,
            PersonaState::Invisible,
        ] {
            assert_eq!(Presence::of(state.clone()).to_state(), state);
        }
    }

    /// Offline is on the list, because it is on Valve's own. What it means is
    /// Steam's meaning: the session leaves the friends network.
    #[test]
    fn offline_is_offered_and_is_the_one_that_is_leaving() {
        assert_eq!(
            CHOOSABLE,
            [
                Presence::Online,
                Presence::Away,
                Presence::Invisible,
                Presence::Offline
            ]
        );
        assert!(Presence::Offline.is_away_from_it_all());
        for standing in [Presence::Online, Presence::Away, Presence::Invisible] {
            assert!(!standing.is_away_from_it_all(), "{standing:?}");
        }
    }

    /// Every status that can be chosen has a verb for Valve's client, and the
    /// four that cannot have none — measured: their verbs are accepted and do
    /// nothing.
    #[test]
    fn only_the_four_have_a_verb_for_valves_client() {
        assert_eq!(Presence::Online.url_verb(), Some("online"));
        assert_eq!(Presence::Away.url_verb(), Some("away"));
        assert_eq!(Presence::Invisible.url_verb(), Some("invisible"));
        assert_eq!(Presence::Offline.url_verb(), Some("offline"));
        for silent in [
            Presence::Busy,
            Presence::Snooze,
            Presence::LookingToTrade,
            Presence::LookingToPlay,
        ] {
            assert_eq!(silent.url_verb(), None, "{silent:?}");
        }
    }

    /// Going offline empties the list and asks about nobody, and coming back
    /// fills it again from the same ids — without waiting for a friends list
    /// Steam only sends once.
    #[test]
    fn offline_empties_the_list_and_keeps_it() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1, 2, 3]), false);
        roll.heard(&[
            persona(7, "Me", PersonaState::Online, None),
            persona(1, "Ann", PersonaState::Online, None),
            persona(2, "Bea", PersonaState::Online, Some("Portal 2")),
            persona(3, "Cal", PersonaState::Offline, None),
        ]);
        assert_eq!(roll.roster().friends.len(), 3);

        assert!(roll.went_offline(true));
        assert!(roll.is_offline());
        let quiet = roll.roster();
        assert!(
            quiet.friends.is_empty(),
            "the list is not shown while offline"
        );
        assert!(
            quiet.me.is_some(),
            "the account itself still has a name and a picture"
        );
        // Nobody is asked about but the account, which is the traffic Offline
        // is for stopping.
        assert_eq!(roll.listed(&listed(&[1, 2, 3]), false), vec![7]);
        // And saying it twice is not a change.
        assert!(!roll.went_offline(true));

        assert!(roll.went_offline(false));
        assert_eq!(
            roll.roster().friends.len(),
            3,
            "the list came back without a reconnect"
        );
        assert_eq!(roll.everyone(), vec![1, 2, 3, 7]);
    }

    /// A session that comes up already offline — the choice outlived the
    /// connection — asks about nobody from the first message.
    #[test]
    fn a_session_that_comes_up_offline_asks_about_nobody() {
        let mut roll = Roll::about_and(7, true);
        assert!(roll.is_offline());
        assert_eq!(roll.listed(&listed(&[1, 2]), false), vec![7]);
        assert!(roll.roster().friends.is_empty());
    }

    /// What this session told Steam shows on the panel before Steam has said it
    /// back — and only where there is a row to correct.
    #[test]
    fn a_status_this_session_chose_is_on_the_panel_at_once() {
        let mut roll = Roll::about(7);
        // Nobody yet: there is no account row to write on, and one invented
        // here would be a nameless person at the head of the panel.
        assert!(!roll.told_steam(Presence::Online));
        assert!(roll.roster().me.is_none());

        roll.listed(&listed(&[1]), false);
        roll.heard(&[persona(7, "Me", PersonaState::Invisible, None)]);
        assert!(roll.told_steam(Presence::Online));
        assert_eq!(
            roll.roster().me.map(|me| me.presence),
            Some(Presence::Online)
        );
        // Saying it twice is not a change, and a redraw nothing moved for is a
        // frame spent on nothing.
        assert!(!roll.told_steam(Presence::Online));
    }

    fn a_connection() -> steam_cm_protocol::connection::ConnectionState {
        steam_cm_protocol::connection::ConnectionState {
            steamid: Some(7),
            client_session_id: Some(3),
            ..Default::default()
        }
    }

    #[test]
    fn the_account_is_asked_about_alongside_its_friends() {
        let mut roll = Roll::about(7);
        let asking = roll.listed(&listed(&[1, 2]), false);
        assert_eq!(asking, vec![1, 2, 7]);
    }

    #[test]
    fn rows_are_a_game_first_then_reachable_then_gone() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1, 2, 3, 4]), false);
        roll.heard(&[
            persona(1, "Zoe", PersonaState::Online, None),
            persona(2, "alice", PersonaState::Offline, None),
            persona(3, "Bob", PersonaState::Online, Some("Half-Life 2")),
            persona(4, "carol", PersonaState::Away, None),
        ]);
        let roster = roll.roster();
        let names: Vec<&str> = roster
            .friends
            .iter()
            .map(|person| person.name.as_str())
            .collect();
        assert_eq!(names, ["Bob", "carol", "Zoe", "alice"]);
        assert_eq!(roster.counts(), [1, 2, 1]);
    }

    #[test]
    fn the_accounts_own_persona_is_not_one_of_its_friends() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1]), false);
        roll.heard(&[
            persona(7, "Me", PersonaState::Online, None),
            persona(1, "Friend", PersonaState::Online, None),
        ]);
        let roster = roll.roster();
        assert_eq!(roster.me.map(|me| me.name), Some("Me".to_string()));
        assert_eq!(roster.friends.len(), 1);
    }

    /// A persona for somebody met in a lobby is not a friend and must not
    /// become a row.
    #[test]
    fn a_stranger_is_not_added_to_the_list() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1]), false);
        assert!(!roll.heard(&[persona(99, "Stranger", PersonaState::Online, None)]));
        assert_eq!(roll.roster().friends.len(), 1);
    }

    /// A push that says nothing about a game leaves the game alone. Without
    /// this a friend who changed their name would step out of the band they
    /// are in.
    #[test]
    fn a_push_without_game_fields_leaves_the_game_where_it_was() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1]), false);
        roll.heard(&[persona(1, "Bob", PersonaState::Online, Some("Half-Life 2"))]);
        roll.heard(&[persona(1, "Bobby", PersonaState::Online, None)]);
        let roster = roll.roster();
        assert_eq!(roster.friends[0].name, "Bobby");
        assert_eq!(roster.friends[0].band(), Band::Playing);
        assert_eq!(roster.friends[0].doing(), "Half-Life 2");
    }

    /// A friend in a game Steam sent no name for is asked about once, and the
    /// row says what is true meanwhile.
    #[test]
    fn a_game_with_no_name_is_asked_about_once() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1, 2]), false);
        // Two friends in the same game, and Steam named neither.
        let in_game = |id| steam_cm_protocol::friends::Persona {
            steamid: id,
            name: format!("friend {id}"),
            state: PersonaState::Online,
            game_app_id: Some(220),
            game_name: None,
            avatar_hash: None,
            game_fields_present: true,
        };
        roll.heard(&[in_game(1), in_game(2)]);
        assert_eq!(roll.roster().friends[0].doing(), "In game");
        assert_eq!(roll.roster().friends[0].band(), Band::Playing);

        // One question for the two of them.
        let wanted = roll.games_to_name();
        assert_eq!(wanted, vec![220]);
        assert!(roll.named(&wanted, BTreeMap::from([(220, "Half-Life 2".to_string())])));
        assert_eq!(roll.roster().friends[0].doing(), "Half-Life 2");
        assert_eq!(roll.roster().friends[1].doing(), "Half-Life 2");
        // And never asked again.
        assert!(roll.games_to_name().is_empty());
    }

    /// One Steam declines to name is not asked about again either, or every
    /// persona push would start another round trip about the same id.
    #[test]
    fn a_game_steam_will_not_name_is_not_asked_about_again() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1]), false);
        roll.heard(&[steam_cm_protocol::friends::Persona {
            steamid: 1,
            name: "Ann".to_string(),
            state: PersonaState::Online,
            game_app_id: Some(999),
            game_name: None,
            avatar_hash: None,
            game_fields_present: true,
        }]);
        let wanted = roll.games_to_name();
        assert_eq!(wanted, vec![999]);
        assert!(!roll.named(&wanted, BTreeMap::new()));
        assert!(roll.games_to_name().is_empty());
        assert_eq!(roll.roster().friends[0].doing(), "In game");
    }

    /// A name Steam sent itself always wins over one that had to be looked up.
    #[test]
    fn the_name_steam_sent_beats_the_one_that_was_looked_up() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1]), false);
        roll.named(&[220], BTreeMap::from([(220, "Half-Life 2".to_string())]));
        roll.heard(&[persona(1, "Ann", PersonaState::Online, Some("Some Mod"))]);
        assert_eq!(roll.roster().friends[0].doing(), "Some Mod");
    }

    #[test]
    fn an_account_with_no_picture_is_asked_for_none() {
        assert_eq!(avatar_of(&[0; 20]), None);
        assert_eq!(avatar_of(&[0xab, 0x0f]), Some("ab0f".to_string()));
    }

    #[test]
    fn an_avatar_is_addressed_at_the_size_it_is_drawn() {
        let person = Person {
            avatar: Some("abc".to_string()),
            ..Person::unnamed(1)
        };
        assert_eq!(
            person.avatar_url(AvatarSize::Medium).as_deref(),
            Some("https://avatars.steamstatic.com/abc_medium.jpg")
        );
        assert_eq!(
            person.avatar_url(AvatarSize::Full).as_deref(),
            Some("https://avatars.steamstatic.com/abc_full.jpg")
        );
        assert_eq!(Person::unnamed(1).avatar_url(AvatarSize::Full), None);
    }

    /// Somebody who stops being a friend leaves the list rather than staying
    /// on it for the session.
    #[test]
    fn a_list_that_arrives_again_replaces_the_one_before_it() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1, 2]), false);
        roll.heard(&[persona(1, "Bob", PersonaState::Online, None)]);
        roll.listed(&listed(&[1]), false);
        let roster = roll.roster();
        assert_eq!(roster.friends.len(), 1);
        assert_eq!(roster.friends[0].name, "Bob");
    }

    #[test]
    fn an_incremental_addition_keeps_every_friend_already_listed() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1, 2]), false);
        roll.heard(&[
            persona(1, "Ann", PersonaState::Online, None),
            persona(2, "Bea", PersonaState::Online, None),
        ]);

        assert_eq!(roll.listed(&listed(&[3]), true), vec![3, 7]);
        assert_eq!(roll.roster().friends.len(), 3);
        assert!(roll
            .roster()
            .friends
            .iter()
            .any(|person| person.name == "Ann"));
        assert!(roll
            .roster()
            .friends
            .iter()
            .any(|person| person.name == "Bea"));
    }

    #[test]
    fn an_incremental_relationship_change_removes_only_that_person() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1, 2, 3]), false);
        let removed = steam_cm_protocol::friends::Friend {
            steamid: 2,
            relationship: 0,
        };

        assert_eq!(roll.listed(&[removed], true), vec![7]);
        let ids: Vec<u64> = roll
            .roster()
            .friends
            .iter()
            .map(|person| person.steam_id)
            .collect();
        assert_eq!(ids, [1, 3]);
    }

    #[test]
    fn an_empty_full_snapshot_clears_the_roster() {
        let mut roll = Roll::about(7);
        roll.listed(&listed(&[1, 2]), false);
        assert_eq!(roll.roster().friends.len(), 2);

        assert_eq!(roll.listed(&[], false), vec![7]);
        assert!(roll.roster().friends.is_empty());
    }
}
