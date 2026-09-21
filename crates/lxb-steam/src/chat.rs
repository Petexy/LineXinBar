//! One-to-one conversations: what has been said, what is still going, and what
//! would not go at all.
//!
//! Steam's friend chat is the unified `FriendMessages.*` service rather than
//! anything on the old `ClientChatMsg` path, and it rides the **same CM session
//! the roster does** — see [`crate::cm`]. There is no second connection, no
//! second sign-in, and nothing here starts Valve's client: a conversation is
//! three request/response calls and one server push on a socket that is already
//! open.
//!
//! ```text
//!   logon (chat_mode = 2) ────────────────────────────────────────► Steam
//!         │                                                          │
//!         ├─ FriendMessages.GetRecentMessages#1 ──► history          │
//!         ├─ FriendMessages.SendMessage#1 ────────► a confirmed key  │
//!         └─ FriendMessagesClient.IncomingMessage#1 ◄────────────────┘
//! ```
//!
//! ## What a message is keyed by, and why it has to be
//!
//! Steam stamps every message it accepts with a **server timestamp and an
//! ordinal**, and the same pair comes back three separate ways: in the answer
//! to the send, in the history the conversation is opened with, and — when the
//! account is signed in somewhere else as well — in the push Steam sends every
//! one of its own sessions. All three are the same message, and a panel that
//! appended each of them would show it three times. So [`Key`] is the identity
//! of a message everywhere in this module, the store is a map keyed by it, and
//! *arriving twice is a no-op rather than something to detect*.
//!
//! The ordinal is what makes that work at all. Steam's timestamp is whole
//! seconds, two messages a moment apart share one, and a key of nothing but the
//! second would collapse them into one line.
//!
//! ## A message that has not been stamped yet
//!
//! Until Steam answers there is no key, so an outgoing message is not in that
//! map: it is a [`Pending`], drawn under everything that has a key, carrying
//! the request it was sent under and whatever went wrong. When the answer comes
//! it is *moved* into the map under Steam's own key — which is what makes the
//! echo and the history copy land on top of it rather than beside it.
//!
//! ## Nothing here is written to a disk
//!
//! Deliberately, and it is the reason [`Conversations`] is held in memory and
//! thrown away on sign-out. Steam keeps the history and will send it again for
//! the asking; a copy of somebody's private conversation on this machine is a
//! thing to be designed and agreed to rather than a side effect of drawing it.
//!
//! ## What is never logged
//!
//! No message body, ever, at any level. Steam ids are logged as the last four
//! digits of the account number and nothing more — see [`short`], which is the
//! one way an id is written down anywhere in this module.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The most a single message may carry.
///
/// **This is the shell's own cap, not a number Steam publishes.** Steam
/// publishes none, and what it does about a long message is refuse it with a
/// bare `EResult` — so the only way to learn anything was to send. Measured
/// against the live service on 2026-09-03, to a second account of the user's
/// own: 4000, 4001 and 5000 characters were all accepted and stamped; 20000 was
/// refused with result 25. The true ceiling is therefore somewhere above 5000
/// and at or below 20000, and it was **not** pinned down — Steam rate-limits
/// sends with result 84 after two in quick succession, so every further probe
/// costs a minute of waiting and a message in somebody's chat log, and the
/// answer would settle a case nobody on a console keyboard will ever reach.
///
/// Four thousand is chosen inside that: comfortably under anything measured to
/// be refused, and far past anything a person types at a television. It is
/// enforced **as the message is typed** rather than at the send — see
/// `crate::friends` in lxb-desktop — so nothing is ever silently cut; the field
/// simply stops taking characters. A message Steam refuses anyway is a visible,
/// retryable failure like any other, and it says which of the two refusals it
/// was: see `crate::cm::what_steam_said`.
///
/// It is a count of `char`s rather than of bytes, so cutting one is done on
/// character boundaries by construction and a message of emoji is counted the
/// way a message of letters is. See [`fit`].
pub const LONGEST_MESSAGE: usize = 4000;

/// How often at most this session tells somebody it is typing.
///
/// A typing notice is a whole round trip and the user is producing one keypress
/// per tenth of a second, so it is throttled rather than sent per keystroke.
/// Five seconds, which is under [`TYPING_LASTS`] by enough that a continuous
/// typist never blinks out at the other end.
pub const TYPING_EVERY: Duration = Duration::from_secs(5);

/// And how long a "typing…" from somebody else stands before it is taken down.
///
/// Steam sends the notice and never sends a retraction: somebody who starts a
/// message and walks away would otherwise be typing for the rest of the
/// session. Fifteen seconds — three of their notices — so a typist who keeps
/// going is never seen to stop.
pub const TYPING_LASTS: Duration = Duration::from_secs(15);

/// How many messages back a conversation is opened with.
pub const HISTORY_WANTED: u32 = 50;

/// Steam's own identity for one message in one conversation.
///
/// The server's timestamp and the ordinal that separates two messages inside
/// the same second, in that order — so the natural ordering of this type is
/// also the order the messages are read in, and a `BTreeMap` keyed by it needs
/// no sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    /// Steam's `rtime32` server timestamp, in whole seconds.
    pub at: u32,
    /// Which message of that second this is.
    pub ordinal: u32,
}

impl Key {
    pub fn new(at: u32, ordinal: u32) -> Key {
        Key { at, ordinal }
    }
}

/// One message, as Steam has stamped it.
///
/// The same shape whichever of the three routes it arrived by, which is the
/// whole point: history, the answer to a send and a cross-session echo are one
/// kind of thing and the store must not be able to tell them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub key: Key,
    pub body: String,
    /// Whether this account wrote it. Taken from Steam's own `local_echo` on a
    /// push and from the author's account id in history — never from comparing
    /// the conversation's id against anything, which is wrong for an echo.
    pub from_me: bool,
}

/// An outgoing message Steam has not stamped yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// Which send this is, so a late answer can be matched to it and a stale
    /// one dropped.
    pub request: u64,
    pub body: String,
    /// Why it did not go, or `None` while it is still going.
    ///
    /// A failed send stays on the screen: it is something the user wrote and
    /// believes they have sent, and taking it away would be losing it silently.
    /// See [`Conversation::retry`].
    pub failed: Option<String>,
}

impl Pending {
    /// Whether Steam has yet to answer about this one.
    pub fn in_flight(&self) -> bool {
        self.failed.is_none()
    }
}

/// Somebody asking this account to join them in a game.
///
/// **Not a message, and deliberately not stored as one.** A message is
/// identified by the stamp Steam puts on it, and an invitation may arrive with
/// no stamp at all — Steam sends these two ways (see
/// `steam_cm_protocol::chat::GameInvite`) and only one of them is a chat
/// entry. So an invitation has an id of this session's own, and its place in
/// the column is a time rather than a key.
///
/// **What it does not carry is the game.** Neither route says which app the
/// connect string is for; that is whatever the person doing the inviting is
/// playing, which is a fact about the roster. It is resolved where the
/// invitation is read — see `crate::cm` — and remembered here, so a card does
/// not go blank the moment they stop playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// This session's own number for it. What a press names — see
    /// [`Mark::Invite`] — because nothing Steam sends can be relied on to
    /// identify one.
    pub id: u64,
    /// Who asked. The conversation it is filed under says the same thing, and
    /// it is here as well so that an invitation handed to the rest of the shell
    /// — to a launch, to an announcement pressed an hour later — is a whole
    /// record rather than half of a pair.
    pub from: u64,
    /// Steam's `rtime32` stamp, or **zero for an invitation that came with
    /// none**, which is the client push. Zero sorts last rather than first: an
    /// unstamped invitation is not from 1970, it is the thing that has just
    /// happened.
    pub at: u32,
    /// The stamp of the message this invitation *is*, where it came through the
    /// chat service. What it is for is the history: Steam sends the same
    /// invitation back as an ordinary row with that key, and a column that drew
    /// both would show the connect string under its own card. See
    /// [`Conversation::lines`].
    pub key: Option<Key>,
    /// What the game is to be started with, verbatim: `+connect_lobby <id>`
    /// for a Steamworks lobby, or whatever else the game defined. Never shown
    /// to anybody — it is an argument, not a sentence.
    pub connect: String,
    /// The game it is for, where the roster could say, and what it is called.
    pub app_id: Option<u32>,
    pub game: Option<String>,
    /// Whether this session has already handed it to Valve's client.
    ///
    /// What the card says, and nothing more: accepting again is allowed, and
    /// has to be — somebody who joined, played and came back is entitled to
    /// press it a second time.
    pub taken: bool,
}

impl Invite {
    /// The lobby this invitation is to, where it is one.
    ///
    /// `+connect_lobby <id>` is what Steam's own matchmaking sends, and it is
    /// the one form worth taking apart: a lobby has a canonical way of being
    /// joined, and everything else is a command line the game gave itself.
    pub fn lobby(&self) -> Option<u64> {
        let rest = self.connect.trim().strip_prefix(CONNECT_LOBBY)?;
        rest.trim().parse().ok()
    }
}

/// The prefix Steam's own lobby invitations are spelled with.
const CONNECT_LOBBY: &str = "+connect_lobby ";

/// Whether a message body is in fact a connect string, and so an invitation
/// that has lost its kind.
///
/// **The history is why this exists.** `GetRecentMessages` answers with rows
/// that carry no entry type at all, so an invitation fetched with a
/// conversation is indistinguishable from somebody having typed its connect
/// string — and drawn as a message it is a line of machinery in a column of
/// sentences. Recognised, it is the same card as the live one, and the two
/// meet under the same connect string.
///
/// Deliberately narrow: one of Steam's two spellings, one argument, nothing
/// else on the line. A body that merely *contains* one of these words is
/// somebody talking about it.
pub fn connect_string(body: &str) -> Option<&str> {
    let body = body.trim();
    let (word, rest) = body.split_once(' ')?;
    if rest.trim().is_empty() || rest.trim().contains(' ') {
        return None;
    }
    match word {
        "+connect_lobby" if rest.trim().chars().all(|c| c.is_ascii_digit()) => Some(body),
        "+connect" => Some(body),
        _ => None,
    }
}

/// How a conversation's history stands.
///
/// Four states rather than "some messages or none", because **an empty
/// conversation and a history that could not be fetched are different things**
/// and a panel that drew both as an empty column would be telling somebody a
/// friend had never written to them when in fact Steam had refused to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum History {
    /// Nobody has asked yet.
    Unasked,
    /// Asked, under this request, and not yet answered.
    Asking(u64),
    /// Steam answered. The messages, however many there were, are in the store.
    Read,
    /// Steam did not answer, and this is what to say about it. Retryable.
    Failed(String),
}

impl History {
    pub fn is_loading(&self) -> bool {
        matches!(self, History::Asking(_))
    }

    /// What went wrong, for a panel that has to offer the way to try again.
    pub fn failure(&self) -> Option<&str> {
        match self {
            History::Failed(why) => Some(why),
            _ => None,
        }
    }
}

/// Everything known about talking to one person.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    /// Everything Steam has stamped, in the order it was said.
    said: BTreeMap<Key, Said>,
    /// And what has been written here and not yet stamped, oldest first.
    pending: Vec<Pending>,
    /// Invitations to a game, oldest first, at most [`INVITES_KEPT`] of them.
    ///
    /// Their own list rather than entries in `said`, because an invitation is
    /// not keyed like a message — see [`Invite`].
    invites: Vec<Invite>,
    history: HistoryState,
    /// When the friend was last seen to be typing, and until when that stands.
    typing_until: Option<Instant>,
    /// When this session last told them *it* was typing, for the throttle.
    told_them_at: Option<Instant>,
    /// How many of their messages have arrived since this conversation was last
    /// read.
    unread: usize,
}

/// `History` with a `Default`, kept private so the public enum needs none.
#[derive(Debug, Clone)]
struct HistoryState(History);

impl Default for HistoryState {
    fn default() -> HistoryState {
        HistoryState(History::Unasked)
    }
}

/// One line of a drawn conversation: something stamped, or something still
/// going out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line<'a> {
    Said(&'a Said),
    Pending(&'a Pending),
    /// Somebody asking this account to join them in a game, which is drawn as a
    /// card rather than as a bubble. See [`Invite`].
    Invite(&'a Invite),
}

impl Line<'_> {
    /// What was written on this line, which for an invitation is **nothing**.
    ///
    /// An invitation's words are the shell's own — a name and a sentence about
    /// it — and the only text it carries is a connect string that must never
    /// reach a screen. Answering with the empty string is what makes every
    /// path that measures or draws a body safe without knowing invitations
    /// exist.
    pub fn body(&self) -> &str {
        match self {
            Line::Said(said) => &said.body,
            Line::Pending(pending) => &pending.body,
            Line::Invite(_) => "",
        }
    }

    /// The invitation on this line, where it is one.
    pub fn invite(&self) -> Option<&Invite> {
        match self {
            Line::Invite(invite) => Some(invite),
            _ => None,
        }
    }

    /// Whether this account wrote it. A pending message is always ours, and an
    /// invitation is only ever somebody else's — one this account sent is
    /// dropped where it is read, because an invitation nobody can accept is not
    /// a line in a conversation.
    pub fn from_me(&self) -> bool {
        match self {
            Line::Said(said) => said.from_me,
            Line::Pending(_) => true,
            Line::Invite(_) => false,
        }
    }

    /// Why it did not go, for the one line that has a reason.
    pub fn failure(&self) -> Option<&str> {
        match self {
            Line::Said(_) | Line::Invite(_) => None,
            Line::Pending(pending) => pending.failed.as_deref(),
        }
    }

    /// Whether Steam has yet to answer about it.
    pub fn sending(&self) -> bool {
        matches!(self, Line::Pending(pending) if pending.in_flight())
    }

    /// Something that identifies this line across a redraw, for a layout that
    /// caches how tall each one is. Stamped messages are keyed by Steam's own
    /// key; a pending one by its request, which no key can collide with because
    /// the two are drawn from different halves of the store.
    pub fn mark(&self) -> Mark {
        match self {
            Line::Said(said) => Mark::Said(said.key),
            Line::Pending(pending) => Mark::Pending(pending.request),
            Line::Invite(invite) => Mark::Invite(invite.id),
        }
    }
}

/// What identifies one drawn line across frames. See [`Line::mark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mark {
    Said(Key),
    Pending(u64),
    /// An invitation, by the number this session gave it. It cannot collide
    /// with a pending message's request although both are `u64`: the two are
    /// different arms, and a mark is compared whole.
    Invite(u64),
}

impl Conversation {
    /// Everything to draw, oldest first: what Steam has stamped, and then what
    /// is still on its way.
    ///
    /// Pending messages go at the end rather than in time order because they
    /// have no time: they are what this person has just written, and the bottom
    /// of the column is where they wrote it.
    pub fn lines(&self) -> Vec<Line<'_>> {
        // Every message an invitation *is*. Steam sends a chat-carried
        // invitation back in the history as an ordinary row, so without this
        // the column would draw the card and the connect string under it.
        let spoken_for = |key: &Key| self.invites.iter().any(|invite| invite.key == Some(*key));
        let mut lines: Vec<Line<'_>> = self
            .said
            .values()
            .filter(|said| !spoken_for(&said.key))
            .map(Line::Said)
            .collect();
        // The invitations, each put where its time says. An unstamped one —
        // the client push carries no time at all — goes to the end, because
        // what it is is the thing that has just happened. Inserted rather than
        // appended-and-sorted so that messages keep the order the store has
        // them in, which is Steam's own.
        for invite in &self.invites {
            let at = match invite.at {
                0 => usize::MAX,
                at => lines
                    .iter()
                    .position(|line| match line {
                        Line::Said(said) => said.key.at > at,
                        _ => false,
                    })
                    .unwrap_or(usize::MAX),
            };
            match at {
                usize::MAX => lines.push(Line::Invite(invite)),
                at => lines.insert(at, Line::Invite(invite)),
            }
        }
        // And what is still going out, which has no time either and is always
        // last: it is what this person has just written.
        lines.extend(self.pending.iter().map(Line::Pending));
        lines
    }

    /// How many lines there are, without building them.
    pub fn len(&self) -> usize {
        let hidden = self
            .said
            .keys()
            .filter(|key| {
                self.invites
                    .iter()
                    .any(|invite| invite.key.as_ref() == Some(key))
            })
            .count();
        self.said.len() - hidden + self.pending.len() + self.invites.len()
    }

    /// The invitations in this conversation, oldest first.
    pub fn invites(&self) -> &[Invite] {
        &self.invites
    }

    /// The one a press acts on: the newest invitation there is.
    ///
    /// Newest by *arrival* rather than by stamp, which is the same thing —
    /// they are held in the order they arrived, and an unstamped one is by
    /// definition the latest thing to have happened. An invitation that has
    /// already been accepted is still the answer: somebody who joined, played
    /// and came back is entitled to press it again, and nothing else in the
    /// column would be a better answer to *accept the invitation*.
    pub fn newest_invite(&self) -> Option<&Invite> {
        self.invites.last()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn history(&self) -> &History {
        &self.history.0
    }

    /// How many of their messages have arrived since this was last read.
    pub fn unread(&self) -> usize {
        self.unread
    }

    /// Whether the friend is typing, as of `now`.
    ///
    /// Read rather than stored, so a "typing…" put up by a notice that arrived
    /// twenty seconds ago goes away on the next frame without anything having
    /// to remember to take it down. See [`TYPING_LASTS`].
    pub fn typing(&self, now: Instant) -> bool {
        self.typing_until.is_some_and(|until| now < until)
    }
}

/// What the shell has said and been told about every conversation on one
/// account.
///
/// **Owned by one account and one CM generation at a time.** Both are recorded
/// rather than assumed, and both are checked before any answer is applied:
/// a send that was still in flight when the account changed must not append its
/// answer to the next person's conversation, and a history fetched over a
/// connection this session has already given up on is a list of what was said
/// before the network dropped.
#[derive(Debug, Default)]
pub struct Conversations {
    /// The account these belong to, or `None` while nobody is signed in.
    account: Option<u64>,
    /// The CM session they were asked over. Answers from any earlier one are
    /// dropped.
    generation: u64,
    with: BTreeMap<u64, Conversation>,
    /// The next request id. Monotonic for the life of the process rather than
    /// per conversation, so an id identifies one request outright and a reply
    /// that arrives after a conversation was closed and reopened cannot be
    /// mistaken for a reply to the second opening.
    next_request: u64,
}

/// Why something cannot be sent, or `None` when it can.
///
/// Answered before a request is made rather than reported after one, because
/// three of the four are things the user can see for themselves and the fourth
/// — a friend who is no longer a friend — is one they cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing was typed, or nothing but spaces.
    Empty,
    /// This account is standing offline on Steam, or the CM is reconnecting.
    NotConnected,
    /// They are not on the friends list any more.
    NotAFriend,
}

impl Refused {
    /// One line, for the panel to put under the compose field.
    pub fn said(self) -> &'static str {
        match self {
            Refused::Empty => "Type a message to send it.",
            Refused::NotConnected => "You are offline on Steam. Messages cannot be sent.",
            Refused::NotAFriend => "You are no longer friends. Messages cannot be sent.",
        }
    }
}

/// What the shell wants of Steam about a conversation.
///
/// Carried out on the CM session's own background tasks — never on the packet
/// loop. See [`crate::cm`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// The last [`HISTORY_WANTED`] messages of one conversation.
    History { with: u64, request: u64 },
    /// One message.
    Send {
        with: u64,
        request: u64,
        body: String,
    },
    /// That this account is typing. Answered by nothing: a typing notice that
    /// did not arrive is not worth a line on the screen.
    Typing { with: u64 },
}

/// One thing Steam had to say about a conversation.
///
/// Every arm carries the CM generation it came from and the account it is
/// about, in [`Heard`], because every one of them is a *side effect on the
/// screen* and none of them may be applied to the wrong account's panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    /// A CM session is up and has opted into chat. What was in flight over the
    /// last one never will be.
    Listening,
    /// The answer to a [`Wanted::History`].
    History {
        with: u64,
        request: u64,
        said: Result<Vec<Said>, String>,
    },
    /// The answer to a [`Wanted::Send`].
    Sent {
        with: u64,
        request: u64,
        said: Result<Said, String>,
    },
    /// Something arrived unasked: a friend's message, or an echo of one of this
    /// account's own sends from another of its sessions.
    Arrived { with: u64, said: Said },
    /// Somebody is typing.
    Typing { with: u64 },
    /// Somebody asked this account to join them in a game.
    Invited { with: u64, invite: Invited },
}

/// An invitation as it arrived, before the store has given it a number.
///
/// The three fields Steam sends and the two the roster answers for. See
/// [`Invite`], which is what this becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invited {
    /// Steam's stamp, or zero where it sent none.
    pub at: u32,
    /// The message it is, where it came through the chat service.
    pub key: Option<Key>,
    pub connect: String,
    pub app_id: Option<u32>,
    pub game: Option<String>,
}

/// A [`Word`], stamped with what it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub generation: u64,
    pub account: u64,
    pub word: Word,
}

/// What applying a [`Heard`] changed, so the shell knows what to redraw and
/// what to announce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moved {
    /// Something on the panel is different.
    pub redraw: bool,
    /// A CM session came up, so every history is stale and the conversation
    /// that is open has to be asked for again — see
    /// [`Conversations::the_connection_was_replaced`]. A conversation nobody is
    /// looking at asks for itself the next time it is opened; this is for the
    /// one on screen, which would otherwise sit there missing everything said
    /// while the network was down.
    pub reconnected: bool,
    /// A message from somebody else arrived for a conversation nobody has open,
    /// and this is who it was from and what it said.
    ///
    /// The body travels with it because the shell decides whether to show it —
    /// see the notification rule — and the decision needs the text to withhold.
    pub announce: Option<(u64, String)>,
    /// And an invitation to a game arrived: who from, and which one.
    ///
    /// The number rather than the invitation itself, because what the shell
    /// does with it needs the store anyway — it has to say what game it is for,
    /// and it has to be able to find it again when somebody presses the
    /// announcement an hour later. See [`Conversation::newest_invite`].
    pub invited: Option<(u64, u64)>,
}

impl Moved {
    fn redrawn() -> Moved {
        Moved {
            redraw: true,
            ..Moved::default()
        }
    }

    fn absorb(&mut self, other: Moved) {
        self.redraw |= other.redraw;
        self.reconnected |= other.reconnected;
        self.announce = self.announce.take().or(other.announce);
        self.invited = self.invited.take().or(other.invited);
    }
}

impl Conversations {
    /// Whose these are.
    pub fn account(&self) -> Option<u64> {
        self.account
    }

    /// The CM session they were asked over.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// One conversation, or nothing where none has been opened.
    pub fn with(&self, friend: u64) -> Option<&Conversation> {
        self.with.get(&friend)
    }

    /// How many unread messages there are altogether.
    pub fn unread(&self) -> usize {
        self.with.values().map(Conversation::unread).sum()
    }

    /// Who has unread messages, and how many each.
    pub fn unread_by(&self) -> Vec<(u64, usize)> {
        self.with
            .iter()
            .filter(|(_, conversation)| conversation.unread > 0)
            .map(|(id, conversation)| (*id, conversation.unread))
            .collect()
    }

    /// Whether anybody is typing at this account right now.
    pub fn anybody_typing(&self, now: Instant) -> bool {
        self.with
            .values()
            .any(|conversation| conversation.typing(now))
    }

    /// Throw everything away, because the account has changed or gone.
    ///
    /// Every conversation, every draft's worth of pending sends, every unread
    /// count and every typing indication — and the generation with them, so
    /// nothing still in flight from the old account can be applied to the new
    /// one. The shell's own draft is cleared beside this; see
    /// `crate::friends` in lxb-desktop.
    pub fn nobody_is_signed_in_now(&mut self) -> bool {
        let had = self.account.is_some() || !self.with.is_empty();
        self.account = None;
        self.with.clear();
        // Not the request counter. It is monotonic for the life of the process
        // on purpose: an answer to a request made by the last account must not
        // be able to match a request id minted for the next one.
        had
    }

    /// This account is now the one these belong to.
    ///
    /// A different account clears everything first. The *same* account signing
    /// in again — which is what a reconnect looks like from here — keeps what
    /// is on the screen, because the messages are still true.
    pub fn signed_in_as(&mut self, account: u64) -> bool {
        if self.account == Some(account) {
            return false;
        }
        self.nobody_is_signed_in_now();
        self.account = Some(account);
        true
    }

    /// Take one thing Steam said. Answers what moved.
    ///
    /// **Everything is checked before anything is applied.** The generation,
    /// the account, the conversation and — where there is one — the request.
    /// A stale answer is dropped in silence: it is not a failure, it is an
    /// answer to a question this session has stopped asking.
    pub fn heard(&mut self, heard: Heard, now: Instant) -> Moved {
        let Heard {
            generation,
            account,
            word,
        } = heard;
        if self.account != Some(account) {
            tracing::debug!(
                account = short(account),
                "a chat answer about another account was dropped"
            );
            return Moved::default();
        }
        if generation < self.generation {
            tracing::debug!(
                generation,
                "a chat answer from an older CM session was dropped"
            );
            return Moved::default();
        }
        let mut moved = Moved::default();
        if generation > self.generation {
            // A new CM session. Nothing that was in flight over the last one
            // can still arrive: its tasks went with it. Every send still saying
            // "sending" would otherwise say it for the rest of the session.
            self.generation = generation;
            moved.absorb(self.the_connection_was_replaced());
            moved.reconnected = true;
        }
        moved.absorb(match word {
            Word::Listening => Moved::default(),
            Word::History {
                with,
                request,
                said,
            } => self.history_came(with, request, said),
            Word::Sent {
                with,
                request,
                said,
            } => self.send_answered(with, request, said),
            Word::Arrived { with, said } => self.arrived(with, said),
            Word::Typing { with } => self.they_are_typing(with, now),
            Word::Invited { with, invite } => self.invited(with, invite),
        });
        moved
    }

    /// Everything in flight has been abandoned by a connection going away.
    ///
    /// Sends fail — visibly, and retryably, because the user wrote them and is
    /// entitled to know they did not go. A history goes back to **unasked**,
    /// whether it had arrived or not, and that is two separate rules wearing one
    /// answer:
    ///
    /// - one that was still coming is not a failure, because nobody was told it
    ///   was coming; the panel simply asks again;
    /// - and one that had *arrived* is now out of date. Steam pushes messages
    ///   at a session, and a session that was not there was pushed nothing —
    ///   so everything said during the outage is missing from a column that
    ///   would otherwise never ask again. Asking again costs one round trip and
    ///   loses nothing: history merges by key, so what is already drawn stays
    ///   exactly where it is.
    fn the_connection_was_replaced(&mut self) -> Moved {
        let mut moved = Moved::default();
        for conversation in self.with.values_mut() {
            for pending in &mut conversation.pending {
                if pending.in_flight() {
                    pending.failed = Some(WHILE_RECONNECTING.to_string());
                    moved.redraw = true;
                }
            }
            if !matches!(conversation.history.0, History::Unasked) {
                conversation.history = HistoryState(History::Unasked);
                moved.redraw = true;
            }
            // And nobody is typing over a connection that has gone.
            conversation.typing_until = None;
            conversation.told_them_at = None;
        }
        moved
    }

    /// Open a conversation: mark it read, and say what to ask Steam for.
    ///
    /// `None` where there is nothing to ask — the history is in hand, a request
    /// for it is already out, or **the last one failed**.
    ///
    /// That last is deliberate, and was the other way round first. A failure
    /// re-asked on every opening is a failure nobody ever sees: the panel says
    /// *Reading the conversation…* again, and a conversation that fails every
    /// time asks Steam again every time anybody looks at it, with nothing to
    /// stop it. So the failure stands, the panel says what went wrong, and
    /// trying again is a press — see [`Self::read_it_again`], which the panel
    /// offers on a line of its own.
    ///
    /// A *reconnect* is not a failure and does re-ask: it puts every history
    /// back to unasked, and the next opening fetches. See
    /// [`Self::the_connection_was_replaced`].
    pub fn open(&mut self, friend: u64) -> Option<Wanted> {
        let request = self.mint();
        let conversation = self.with.entry(friend).or_default();
        conversation.unread = 0;
        match conversation.history.0 {
            History::Read | History::Asking(_) | History::Failed(_) => None,
            History::Unasked => {
                conversation.history = HistoryState(History::Asking(request));
                Some(Wanted::History {
                    with: friend,
                    request,
                })
            }
        }
    }

    /// Ask for the history again after one that failed.
    pub fn read_it_again(&mut self, friend: u64) -> Option<Wanted> {
        let request = self.mint();
        let conversation = self.with.entry(friend).or_default();
        if conversation.history.0.is_loading() {
            return None;
        }
        conversation.history = HistoryState(History::Asking(request));
        Some(Wanted::History {
            with: friend,
            request,
        })
    }

    /// Mark one conversation read without asking Steam anything.
    pub fn read(&mut self, friend: u64) -> bool {
        let Some(conversation) = self.with.get_mut(&friend) else {
            return false;
        };
        let was = conversation.unread;
        conversation.unread = 0;
        was > 0
    }

    /// Whether a message may be sent to `friend` at all, and if not, why.
    ///
    /// `a_friend` is whether Steam still lists them, and `connected` whether
    /// this session is in a state to send. Both are the caller's to answer:
    /// this module holds conversations, not the roster.
    pub fn may_send(body: &str, a_friend: bool, connected: bool) -> Result<String, Refused> {
        let body = fit(body);
        if body.is_empty() {
            return Err(Refused::Empty);
        }
        if !a_friend {
            return Err(Refused::NotAFriend);
        }
        if !connected {
            return Err(Refused::NotConnected);
        }
        Ok(body)
    }

    /// Put one message on its way. The body has already been through
    /// [`Self::may_send`].
    pub fn send(&mut self, friend: u64, body: String) -> Wanted {
        let request = self.mint();
        let conversation = self.with.entry(friend).or_default();
        conversation.pending.push(Pending {
            request,
            body: body.clone(),
            failed: None,
        });
        // Sending is being in the conversation, so it is read.
        conversation.unread = 0;
        Wanted::Send {
            with: friend,
            request,
            body,
        }
    }

    /// Try a failed send again, under a fresh request.
    ///
    /// The message keeps its place in the column: it is the same thing the user
    /// wrote, going out a second time, and moving it to the bottom would look
    /// like a message they did not write.
    pub fn retry(&mut self, friend: u64, request: u64) -> Option<Wanted> {
        let fresh = self.mint();
        let conversation = self.with.get_mut(&friend)?;
        let pending = conversation
            .pending
            .iter_mut()
            .find(|pending| pending.request == request)?;
        if pending.in_flight() {
            return None;
        }
        pending.failed = None;
        pending.request = fresh;
        Some(Wanted::Send {
            with: friend,
            request: fresh,
            body: pending.body.clone(),
        })
    }

    /// Say that this account is typing to `friend`, if it is time to say it
    /// again.
    ///
    /// `None` means the throttle swallowed it, which is the ordinary answer:
    /// somebody typing at speed produces one of these every five seconds and
    /// forty keystrokes in between. See [`TYPING_EVERY`].
    pub fn typing_at(&mut self, friend: u64, now: Instant) -> Option<Wanted> {
        let conversation = self.with.entry(friend).or_default();
        if conversation
            .told_them_at
            .is_some_and(|told| now.saturating_duration_since(told) < TYPING_EVERY)
        {
            return None;
        }
        conversation.told_them_at = Some(now);
        Some(Wanted::Typing { with: friend })
    }

    /// Forget that this account was typing, so the next keystroke says so at
    /// once. What sending a message does.
    pub fn stopped_typing(&mut self, friend: u64) {
        if let Some(conversation) = self.with.get_mut(&friend) {
            conversation.told_them_at = None;
        }
    }

    fn mint(&mut self) -> u64 {
        self.next_request = self.next_request.wrapping_add(1).max(1);
        self.next_request
    }

    fn history_came(
        &mut self,
        friend: u64,
        request: u64,
        said: Result<Vec<Said>, String>,
    ) -> Moved {
        let Some(conversation) = self.with.get_mut(&friend) else {
            return Moved::default();
        };
        // The request, and not merely the conversation. A conversation opened,
        // closed and opened again has two fetches out; the first to come back
        // is not necessarily the first that was asked for, and applying an
        // older one over a newer would put an older history on the screen.
        if conversation.history.0 != History::Asking(request) {
            tracing::debug!(
                with = short(friend),
                "a history answer nothing was waiting for was dropped"
            );
            return Moved::default();
        }
        match said {
            Ok(said) => {
                conversation.history = HistoryState(History::Read);
                // An invitation comes back from the history as an ordinary row
                // — `GetRecentMessages` carries no entry type — so the ones in
                // it are recognised by their bodies and filed as invitations
                // like any other. See [`connect_string`]. Gathered rather than
                // filed here, because filing one needs a number and the store
                // is borrowed.
                let mut invitations = Vec::new();
                for one in said {
                    if !one.from_me
                        && connect_string(&one.body).is_some()
                        && !conversation
                            .invites
                            .iter()
                            .any(|invite| invite.connect == one.body.trim())
                    {
                        invitations.push(Invited {
                            at: one.key.at,
                            key: Some(one.key),
                            connect: one.body.trim().to_string(),
                            app_id: None,
                            game: None,
                        });
                    }
                    // Merged rather than replacing: messages that arrived live
                    // while the fetch was out are already here under the same
                    // keys, and anything newer than the history must survive
                    // it.
                    conversation.said.insert(one.key, one);
                }
                for invitation in invitations {
                    // Not announced, and deliberately: these are not arrivals.
                    // The conversation is being read *now*, and the newest of
                    // them may be older than the session.
                    self.invited(friend, invitation);
                }
            }
            Err(why) => {
                tracing::warn!(with = short(friend), %why, "Steam would not send the history");
                conversation.history = HistoryState(History::Failed(why));
            }
        }
        Moved::redrawn()
    }

    fn send_answered(&mut self, friend: u64, request: u64, said: Result<Said, String>) -> Moved {
        let Some(conversation) = self.with.get_mut(&friend) else {
            return Moved::default();
        };
        let Some(at) = conversation
            .pending
            .iter()
            .position(|pending| pending.request == request)
        else {
            tracing::debug!(
                with = short(friend),
                "an answer to a send nothing was waiting for was dropped"
            );
            return Moved::default();
        };
        match said {
            Ok(said) => {
                // Off the pending list and into the store under Steam's own
                // key, which is what makes the echo and the history copy of
                // this same message land on top of it rather than beside it.
                conversation.pending.remove(at);
                conversation.said.insert(said.key, said);
            }
            Err(why) => {
                tracing::warn!(with = short(friend), %why, "Steam would not take a message");
                conversation.pending[at].failed = Some(why);
            }
        }
        Moved::redrawn()
    }

    fn arrived(&mut self, friend: u64, said: Said) -> Moved {
        let from_me = said.from_me;
        let body = said.body.clone();
        let conversation = self.with.entry(friend).or_default();
        // Already known is the ordinary case for an echo of this session's own
        // send: it went into the store under this key when Steam answered.
        let fresh = conversation.said.insert(said.key, said).is_none();
        if !fresh {
            return Moved::default();
        }
        // An echo of this account's own message, written somewhere else, is not
        // unread and is nothing to announce: the person who wrote it is the
        // person who would be told.
        if from_me {
            return Moved::redrawn();
        }
        conversation.unread = conversation.unread.saturating_add(1);
        // Somebody who has sent a message has stopped typing.
        conversation.typing_until = None;
        Moved {
            redraw: true,
            announce: Some((friend, body)),
            ..Moved::default()
        }
    }

    /// File an invitation that has just arrived.
    ///
    /// **The connect string is its identity.** Steam has two ways of sending
    /// one and this session reads both, so the same invitation may arrive
    /// twice within a millisecond; and the history brings the chat entry of one
    /// that is already in hand. Two cards to the same lobby would be the shell
    /// telling somebody they had been asked twice.
    ///
    /// What a repeat is allowed to do is fill gaps: a stamp where there was
    /// none, a newer time, the game where the roster had not answered yet.
    /// What it must never do is un-take one — an invitation this session has
    /// already handed to Valve's client is not new work, and a card that went
    /// back to saying *Accept* would be the second push of a pair undoing the
    /// press that landed between them.
    fn invited(&mut self, friend: u64, invited: Invited) -> Moved {
        let id = self.mint();
        let conversation = self.with.entry(friend).or_default();
        if let Some(held) = conversation
            .invites
            .iter_mut()
            .find(|held| held.connect == invited.connect)
        {
            let mut redraw = false;
            if held.key.is_none() && invited.key.is_some() {
                held.key = invited.key;
                // The card does not change, but the column does: the message
                // this invitation *is* stops being drawn under it.
                redraw = true;
            }
            if invited.at > held.at {
                held.at = invited.at;
            }
            if held.app_id.is_none() && invited.app_id.is_some() {
                held.app_id = invited.app_id;
                held.game = invited.game;
                redraw = true;
            }
            return Moved {
                redraw,
                ..Moved::default()
            };
        }
        conversation.invites.push(Invite {
            id,
            from: friend,
            at: invited.at,
            key: invited.key,
            connect: invited.connect,
            app_id: invited.app_id,
            game: invited.game,
            taken: false,
        });
        // A column of invitations is not a conversation. The oldest go, which
        // are the ones whose lobbies have long since closed.
        while conversation.invites.len() > INVITES_KEPT {
            conversation.invites.remove(0);
        }
        // Counted with the messages, because the bead on somebody's row
        // answers *is there anything here for me* and an invitation is the
        // most of anything there is.
        conversation.unread = conversation.unread.saturating_add(1);
        conversation.typing_until = None;
        Moved {
            redraw: true,
            invited: Some((friend, id)),
            ..Moved::default()
        }
    }

    /// Say that an invitation has been handed to Valve's client, and give back
    /// what to hand over.
    ///
    /// The whole of what *accepting* means in this crate: nothing here talks to
    /// Steam, starts anything, or knows what a game is. The shell takes the
    /// connect string from here and everything else is its business.
    pub fn take_the_invite(&mut self, friend: u64, id: u64) -> Option<Invite> {
        let conversation = self.with.get_mut(&friend)?;
        let invite = conversation
            .invites
            .iter_mut()
            .find(|invite| invite.id == id)?;
        invite.taken = true;
        Some(invite.clone())
    }

    fn they_are_typing(&mut self, friend: u64, now: Instant) -> Moved {
        let conversation = self.with.entry(friend).or_default();
        conversation.typing_until = Some(now + TYPING_LASTS);
        Moved::redrawn()
    }
}

/// What a send that a reconnect abandoned is told.
const WHILE_RECONNECTING: &str = "Steam reconnected before this was sent.";

/// How many invitations one conversation keeps.
///
/// Small on purpose. An invitation is a thing to act on now — the lobby behind
/// an old one is not there any more — and a column that filled up with them
/// would be a conversation nobody could read.
const INVITES_KEPT: usize = 8;

/// Trim a message and cut it to Steam's limit, on a character boundary.
///
/// Trimmed at both ends because trailing whitespace is not something anybody
/// means to send, and cut by *characters* rather than bytes — see
/// [`LONGEST_MESSAGE`]. A byte cut would split a multi-byte character and
/// produce a message that is not valid UTF-8, which `String` will not even
/// hold; taking the first `LONGEST_MESSAGE` `char`s cannot.
pub fn fit(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= LONGEST_MESSAGE {
        return body.to_string();
    }
    body.chars().take(LONGEST_MESSAGE).collect()
}

/// How much of a Steam id may be written down: the last four digits.
///
/// Enough to tell one conversation from another in a log somebody is reading
/// beside their own screen, and not an identifier — the same measure the rest
/// of this crate writes account numbers under. A whole SteamID in a log is a
/// profile anybody who sees the file can open.
pub fn short(steam_id: u64) -> u64 {
    steam_id % 10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u64 = 76_561_198_000_000_001;
    const THEM: u64 = 76_561_198_000_000_002;
    const SOMEBODY_ELSE: u64 = 76_561_198_000_000_003;

    fn said(at: u32, ordinal: u32, body: &str, from_me: bool) -> Said {
        Said {
            key: Key::new(at, ordinal),
            body: body.to_string(),
            from_me,
        }
    }

    /// An account signed in with a CM session up, which is the state every one
    /// of these starts from. The `Listening` is not decoration: it is what
    /// settles the generation, and a store that had never heard one would treat
    /// the first answer as a reconnect.
    fn signed_in() -> Conversations {
        let mut conversations = Conversations::default();
        conversations.signed_in_as(ME);
        conversations.heard(
            Heard {
                generation: 1,
                account: ME,
                word: Word::Listening,
            },
            Instant::now(),
        );
        conversations
    }

    fn hear(conversations: &mut Conversations, word: Word) -> Moved {
        conversations.heard(
            Heard {
                generation: conversations.generation(),
                account: ME,
                word,
            },
            Instant::now(),
        )
    }

    /// The whole of the dedup rule in one test: the same message arriving by
    /// all three routes is drawn once.
    #[test]
    fn one_message_by_three_routes_is_one_line() {
        let mut conversations = signed_in();
        let Wanted::Send { request, .. } = conversations.send(THEM, "hello".to_string()) else {
            panic!("a send");
        };
        hear(
            &mut conversations,
            Word::Sent {
                with: THEM,
                request,
                said: Ok(said(100, 0, "hello", true)),
            },
        );
        // The echo Steam sends this account's other sessions, which this one
        // also receives.
        hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(100, 0, "hello", true),
            },
        );
        // And the copy that comes back in the history a moment later.
        let Some(Wanted::History { request, .. }) = conversations.read_it_again(THEM) else {
            panic!("a history request");
        };
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request,
                said: Ok(vec![said(100, 0, "hello", true)]),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(conversation.len(), 1, "one message, three arrivals");
        assert_eq!(conversation.lines()[0].body(), "hello");
        assert!(!conversation.lines()[0].sending());
    }

    /// Two messages inside one second keep their order and both survive.
    #[test]
    fn two_messages_in_one_second_are_told_apart_by_the_ordinal() {
        let mut conversations = signed_in();
        let Some(Wanted::History { request, .. }) = conversations.open(THEM) else {
            panic!("a history request");
        };
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request,
                said: Ok(vec![
                    said(100, 1, "second", false),
                    said(100, 0, "first", false),
                    said(99, 7, "earlier", false),
                ]),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        let lines = conversation.lines();
        let bodies: Vec<&str> = lines.iter().map(|line| line.body()).collect();
        assert_eq!(bodies, ["earlier", "first", "second"]);
    }

    /// An empty history and a failed one are different states, and only one of
    /// them can be retried.
    #[test]
    fn an_empty_history_is_not_a_failed_one() {
        let mut conversations = signed_in();
        let Some(Wanted::History { request, .. }) = conversations.open(THEM) else {
            panic!("a history request");
        };
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request,
                said: Ok(Vec::new()),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(*conversation.history(), History::Read);
        assert!(conversation.history().failure().is_none());
        assert!(conversation.is_empty());
        // Opening it again asks nothing: the answer is in hand and it is that
        // nothing has been said.
        assert!(conversations.open(THEM).is_none());

        let mut conversations = signed_in();
        let Some(Wanted::History { request, .. }) = conversations.open(SOMEBODY_ELSE) else {
            panic!("a history request");
        };
        hear(
            &mut conversations,
            Word::History {
                with: SOMEBODY_ELSE,
                request,
                said: Err("Steam did not answer".to_string()),
            },
        );
        let conversation = conversations.with(SOMEBODY_ELSE).expect("a conversation");
        assert_eq!(
            conversation.history().failure(),
            Some("Steam did not answer")
        );
        // And opening it again does **not** quietly ask again: the failure
        // stands until somebody presses the retry, or it would be a state
        // nobody ever sees and a conversation that asks Steam every time it is
        // looked at.
        assert!(conversations.open(SOMEBODY_ELSE).is_none());
        assert_eq!(
            conversations
                .with(SOMEBODY_ELSE)
                .expect("a conversation")
                .history()
                .failure(),
            Some("Steam did not answer")
        );
        // The press is what asks.
        assert!(matches!(
            conversations.read_it_again(SOMEBODY_ELSE),
            Some(Wanted::History { .. })
        ));
    }

    /// An out-of-order pair of history answers: the older one is ignored.
    #[test]
    fn a_history_answer_to_an_older_request_is_dropped() {
        let mut conversations = signed_in();
        let Some(Wanted::History { request: first, .. }) = conversations.open(THEM) else {
            panic!("a history request");
        };
        // The first fetch fails, and the retry asks again.
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request: first,
                said: Err("no".to_string()),
            },
        );
        let Some(Wanted::History {
            request: second, ..
        }) = conversations.read_it_again(THEM)
        else {
            panic!("a second history request");
        };
        assert_ne!(first, second);
        // The first answer arrives late.
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request: first,
                said: Ok(vec![said(1, 0, "stale", false)]),
            },
        );
        assert!(
            conversations.with(THEM).expect("a conversation").is_empty(),
            "the late answer to the abandoned request was applied"
        );
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request: second,
                said: Ok(vec![said(2, 0, "fresh", false)]),
            },
        );
        assert_eq!(
            conversations.with(THEM).expect("a conversation").lines()[0].body(),
            "fresh"
        );
    }

    /// A failed send stays on the screen, says why, and goes again on a retry.
    #[test]
    fn a_failed_send_is_visible_and_retryable() {
        let mut conversations = signed_in();
        let Wanted::Send { request, .. } = conversations.send(THEM, "hello".to_string()) else {
            panic!("a send");
        };
        hear(
            &mut conversations,
            Word::Sent {
                with: THEM,
                request,
                said: Err("Steam would not take it".to_string()),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        let lines = conversation.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].failure(), Some("Steam would not take it"));
        assert!(!lines[0].sending());

        let Some(Wanted::Send {
            request: again,
            body,
            ..
        }) = conversations.retry(THEM, request)
        else {
            panic!("a retry");
        };
        assert_eq!(body, "hello");
        assert_ne!(again, request);
        let conversation = conversations.with(THEM).expect("a conversation");
        assert!(conversation.lines()[0].sending(), "it is going again");
        assert_eq!(conversation.len(), 1, "the retry is the same message");

        hear(
            &mut conversations,
            Word::Sent {
                with: THEM,
                request: again,
                said: Ok(said(500, 0, "hello", true)),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(conversation.len(), 1);
        assert!(!conversation.lines()[0].sending());
    }

    /// A send answered under a request that has already been retried is
    /// dropped: the retry owns the message now.
    #[test]
    fn a_late_answer_to_a_retried_send_is_dropped() {
        let mut conversations = signed_in();
        let Wanted::Send { request, .. } = conversations.send(THEM, "hello".to_string()) else {
            panic!("a send");
        };
        hear(
            &mut conversations,
            Word::Sent {
                with: THEM,
                request,
                said: Err("no".to_string()),
            },
        );
        let Some(Wanted::Send { request: again, .. }) = conversations.retry(THEM, request) else {
            panic!("a retry");
        };
        // The first attempt's answer turns up after all.
        hear(
            &mut conversations,
            Word::Sent {
                with: THEM,
                request,
                said: Ok(said(1, 0, "hello", true)),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert!(
            conversation.lines()[0].sending(),
            "the retry is still the one in flight"
        );
        hear(
            &mut conversations,
            Word::Sent {
                with: THEM,
                request: again,
                said: Ok(said(2, 0, "hello", true)),
            },
        );
        assert_eq!(conversations.with(THEM).expect("a conversation").len(), 1);
    }

    /// Nothing about account A may be applied to account B's conversations.
    #[test]
    fn an_answer_about_another_account_is_dropped() {
        let mut conversations = signed_in();
        let Wanted::Send { request, .. } = conversations.send(THEM, "hello".to_string()) else {
            panic!("a send");
        };
        let moved = conversations.heard(
            Heard {
                generation: 1,
                account: SOMEBODY_ELSE,
                word: Word::Sent {
                    with: THEM,
                    request,
                    said: Ok(said(1, 0, "hello", true)),
                },
            },
            Instant::now(),
        );
        assert!(!moved.redraw);
        assert!(
            conversations.with(THEM).expect("a conversation").lines()[0].sending(),
            "somebody else's answer confirmed this account's send"
        );
    }

    /// And an answer from a CM session that has been replaced.
    #[test]
    fn an_answer_from_an_older_cm_session_is_dropped() {
        let mut conversations = signed_in();
        let Wanted::Send { request, .. } = conversations.send(THEM, "hello".to_string()) else {
            panic!("a send");
        };
        // The connection is replaced, which fails what was in flight.
        conversations.heard(
            Heard {
                generation: 5,
                account: ME,
                word: Word::Listening,
            },
            Instant::now(),
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(conversation.lines()[0].failure(), Some(WHILE_RECONNECTING));

        // The old session's answer arrives afterwards.
        conversations.heard(
            Heard {
                generation: 1,
                account: ME,
                word: Word::Sent {
                    with: THEM,
                    request,
                    said: Ok(said(1, 0, "hello", true)),
                },
            },
            Instant::now(),
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(
            conversation.lines()[0].failure(),
            Some(WHILE_RECONNECTING),
            "an answer from the dead session was applied"
        );
    }

    /// A history that was in flight when the connection went is asked again
    /// rather than reported as a failure nobody was waiting for — and so is one
    /// that had already arrived, because nothing said during the outage was
    /// pushed at a session that was not there.
    #[test]
    fn a_reconnect_leaves_every_history_askable() {
        let mut conversations = signed_in();
        // One still coming.
        assert!(conversations.open(THEM).is_some());
        // And one that arrived.
        let Some(Wanted::History { request, .. }) = conversations.open(SOMEBODY_ELSE) else {
            panic!("a history request");
        };
        hear(
            &mut conversations,
            Word::History {
                with: SOMEBODY_ELSE,
                request,
                said: Ok(vec![said(100, 0, "before the outage", false)]),
            },
        );
        let moved = conversations.heard(
            Heard {
                generation: 9,
                account: ME,
                word: Word::Listening,
            },
            Instant::now(),
        );
        assert!(moved.reconnected);
        assert!(
            matches!(conversations.open(THEM), Some(Wanted::History { .. })),
            "a history that was in flight was left in a state nothing would ask again"
        );
        assert!(
            matches!(
                conversations.open(SOMEBODY_ELSE),
                Some(Wanted::History { .. })
            ),
            "a history that had arrived was never asked for again"
        );
        // And nothing already drawn was thrown away to do it.
        assert_eq!(
            conversations
                .with(SOMEBODY_ELSE)
                .expect("a conversation")
                .lines()[0]
                .body(),
            "before the outage"
        );
    }

    /// Nobody is typing over a connection that has gone.
    #[test]
    fn a_reconnect_puts_every_typing_indication_down() {
        let mut conversations = signed_in();
        let now = Instant::now();
        conversations.heard(
            Heard {
                generation: 1,
                account: ME,
                word: Word::Typing { with: THEM },
            },
            now,
        );
        assert!(conversations
            .with(THEM)
            .expect("a conversation")
            .typing(now));
        conversations.heard(
            Heard {
                generation: 2,
                account: ME,
                word: Word::Listening,
            },
            now,
        );
        assert!(!conversations
            .with(THEM)
            .expect("a conversation")
            .typing(now));
        // And this session may say it is typing again at once, rather than
        // waiting out a throttle that belonged to a connection that has gone.
        assert!(conversations.typing_at(THEM, now).is_some());
    }

    /// Signing out takes everything with it, including what is in flight.
    #[test]
    fn signing_out_clears_every_conversation() {
        let mut conversations = signed_in();
        conversations.send(THEM, "hello".to_string());
        hear(
            &mut conversations,
            Word::Arrived {
                with: SOMEBODY_ELSE,
                said: said(1, 0, "hi", false),
            },
        );
        assert_eq!(conversations.unread(), 1);
        assert!(conversations.nobody_is_signed_in_now());
        assert!(conversations.with(THEM).is_none());
        assert!(conversations.with(SOMEBODY_ELSE).is_none());
        assert_eq!(conversations.unread(), 0);
        assert!(conversations.account().is_none());
    }

    /// And so does one account replacing another.
    #[test]
    fn a_different_account_replaces_everything() {
        let mut conversations = signed_in();
        conversations.send(THEM, "hello".to_string());
        assert!(conversations.signed_in_as(SOMEBODY_ELSE));
        assert!(conversations.with(THEM).is_none());
        assert_eq!(conversations.account(), Some(SOMEBODY_ELSE));
        // The same account again is not a change and keeps what is drawn.
        conversations.send(THEM, "again".to_string());
        assert!(!conversations.signed_in_as(SOMEBODY_ELSE));
        assert_eq!(conversations.with(THEM).expect("a conversation").len(), 1);
    }

    /// The throttle: one notice, then silence until the interval is up.
    #[test]
    fn typing_notices_are_throttled() {
        let mut conversations = signed_in();
        let now = Instant::now();
        assert!(conversations.typing_at(THEM, now).is_some());
        assert!(conversations.typing_at(THEM, now).is_none());
        assert!(conversations
            .typing_at(THEM, now + TYPING_EVERY - Duration::from_millis(1))
            .is_none());
        assert!(conversations.typing_at(THEM, now + TYPING_EVERY).is_some());
        // A message sent releases it, so the next keystroke says so at once.
        conversations.stopped_typing(THEM);
        assert!(conversations.typing_at(THEM, now + TYPING_EVERY).is_some());
    }

    /// And the other end of it: somebody's "typing…" goes away on its own.
    #[test]
    fn a_typing_indication_expires() {
        let mut conversations = signed_in();
        let now = Instant::now();
        conversations.heard(
            Heard {
                generation: 1,
                account: ME,
                word: Word::Typing { with: THEM },
            },
            now,
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert!(conversation.typing(now));
        assert!(conversation.typing(now + TYPING_LASTS - Duration::from_millis(1)));
        assert!(!conversation.typing(now + TYPING_LASTS));
    }

    /// A message from somebody puts their typing indication down: they have
    /// finished the thing they were typing.
    #[test]
    fn a_message_ends_the_typing_indication() {
        let mut conversations = signed_in();
        let now = Instant::now();
        conversations.heard(
            Heard {
                generation: 1,
                account: ME,
                word: Word::Typing { with: THEM },
            },
            now,
        );
        conversations.heard(
            Heard {
                generation: 1,
                account: ME,
                word: Word::Arrived {
                    with: THEM,
                    said: said(1, 0, "hi", false),
                },
            },
            now,
        );
        assert!(!conversations
            .with(THEM)
            .expect("a conversation")
            .typing(now));
    }

    /// Unread counting, and what opening a conversation does to it.
    #[test]
    fn unread_counts_their_messages_and_opening_clears_them() {
        let mut conversations = signed_in();
        for ordinal in 0..3 {
            hear(
                &mut conversations,
                Word::Arrived {
                    with: THEM,
                    said: said(10, ordinal, "hi", false),
                },
            );
        }
        assert_eq!(conversations.unread(), 3);
        assert_eq!(conversations.unread_by(), vec![(THEM, 3)]);
        conversations.open(THEM);
        assert_eq!(conversations.unread(), 0);
    }

    /// An echo of this account's own message is neither unread nor announced.
    #[test]
    fn an_echo_of_our_own_message_is_not_unread() {
        let mut conversations = signed_in();
        let moved = hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(10, 0, "from my phone", true),
            },
        );
        assert!(moved.redraw);
        assert!(moved.announce.is_none());
        assert_eq!(conversations.unread(), 0);
        assert_eq!(conversations.with(THEM).expect("a conversation").len(), 1);
    }

    /// A message that is already known announces nothing a second time.
    #[test]
    fn a_repeated_arrival_announces_once() {
        let mut conversations = signed_in();
        let first = hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(10, 0, "hi", false),
            },
        );
        assert!(first.announce.is_some());
        let again = hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(10, 0, "hi", false),
            },
        );
        assert!(again.announce.is_none());
        assert!(!again.redraw);
        assert_eq!(conversations.unread(), 1);
    }

    /// Nothing empty goes out, and nothing goes to somebody who is not a
    /// friend or from a session that cannot send.
    #[test]
    fn what_may_not_be_sent() {
        assert_eq!(Conversations::may_send("", true, true), Err(Refused::Empty));
        assert_eq!(
            Conversations::may_send("   \n\t ", true, true),
            Err(Refused::Empty)
        );
        assert_eq!(
            Conversations::may_send("hello", false, true),
            Err(Refused::NotAFriend)
        );
        assert_eq!(
            Conversations::may_send("hello", true, false),
            Err(Refused::NotConnected)
        );
        assert_eq!(
            Conversations::may_send("  hello  ", true, true),
            Ok("hello".to_string())
        );
    }

    /// The limit is characters, and a cut never splits one.
    #[test]
    fn a_long_message_is_cut_on_a_character_boundary() {
        // Four-byte characters, so a byte-counted cut would land inside one.
        let long: String = std::iter::repeat_n('🙂', LONGEST_MESSAGE + 100).collect();
        let cut = fit(&long);
        assert_eq!(cut.chars().count(), LONGEST_MESSAGE);
        assert_eq!(cut.len(), LONGEST_MESSAGE * 4, "the cut split a character");
        // And one exactly at the limit is untouched.
        let exact: String = std::iter::repeat_n('é', LONGEST_MESSAGE).collect();
        assert_eq!(fit(&exact), exact);
    }

    /// Rapid switching between conversations: each answer lands in its own.
    #[test]
    fn answers_land_in_the_conversation_they_belong_to() {
        let mut conversations = signed_in();
        let Some(Wanted::History {
            request: theirs, ..
        }) = conversations.open(THEM)
        else {
            panic!("a history request");
        };
        let Some(Wanted::History { request: other, .. }) = conversations.open(SOMEBODY_ELSE) else {
            panic!("a history request");
        };
        // Answered in the other order.
        hear(
            &mut conversations,
            Word::History {
                with: SOMEBODY_ELSE,
                request: other,
                said: Ok(vec![said(1, 0, "from the other", false)]),
            },
        );
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request: theirs,
                said: Ok(vec![said(1, 0, "from them", false)]),
            },
        );
        assert_eq!(
            conversations.with(THEM).expect("a conversation").lines()[0].body(),
            "from them"
        );
        assert_eq!(
            conversations
                .with(SOMEBODY_ELSE)
                .expect("a conversation")
                .lines()[0]
                .body(),
            "from the other"
        );
    }

    // -- invitations -------------------------------------------------------

    fn invited(at: u32, connect: &str) -> Word {
        Word::Invited {
            with: THEM,
            invite: Invited {
                at,
                key: (at != 0).then(|| Key::new(at, 0)),
                connect: connect.to_string(),
                app_id: Some(220),
                game: Some("Half-Life 2".to_string()),
            },
        }
    }

    /// An invitation is a line in the conversation, a count against the bead,
    /// and something to announce — and it is **not** a message: it has an id of
    /// this session's own, because the two routes Steam sends one by do not
    /// agree on whether it is stamped at all.
    #[test]
    fn an_invitation_is_a_line_of_its_own() {
        let mut conversations = signed_in();
        let moved = hear(
            &mut conversations,
            invited(1_700_000_100, "+connect_lobby 7"),
        );
        let (from, id) = moved.invited.expect("an invitation to announce");
        assert_eq!(from, THEM);
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(conversation.unread(), 1);
        let invite = conversation.newest_invite().expect("the invitation");
        assert_eq!(invite.id, id);
        assert_eq!(invite.from, THEM);
        assert_eq!(invite.app_id, Some(220));
        assert_eq!(invite.lobby(), Some(7));
        assert!(!invite.taken);
        // One line, and its body is empty: the connect string is an argument
        // and never anything anybody reads.
        let lines = conversation.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].mark(), Mark::Invite(id));
        assert_eq!(lines[0].body(), "");
        assert!(!lines[0].from_me());
    }

    /// Steam has two ways of sending one and this session reads both, so the
    /// same invitation can arrive twice inside a millisecond. The connect
    /// string is what says they are the same, and the second fills in what the
    /// first did not carry rather than making a second card.
    #[test]
    fn the_same_invitation_twice_is_one_invitation() {
        let mut conversations = signed_in();
        // The client push first: no stamp at all.
        hear(&mut conversations, invited(0, "+connect_lobby 7"));
        // Then the chat service's copy of it, which is stamped.
        let again = hear(
            &mut conversations,
            invited(1_700_000_100, "+connect_lobby 7"),
        );
        assert_eq!(again.invited, None, "the second is not a second arrival");
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(conversation.invites().len(), 1);
        let invite = conversation.newest_invite().expect("the invitation");
        assert_eq!(
            invite.at, 1_700_000_100,
            "the stamp is taken from the later"
        );
        assert_eq!(invite.key, Some(Key::new(1_700_000_100, 0)));
        // And the unread count moved once, not twice.
        assert_eq!(conversation.unread(), 1);

        // A different lobby is a different invitation.
        hear(
            &mut conversations,
            invited(1_700_000_200, "+connect_lobby 9"),
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(conversation.invites().len(), 2);
        assert_eq!(
            conversation.newest_invite().expect("the newest").lobby(),
            Some(9)
        );
    }

    /// An invitation that has been accepted stays accepted when the other half
    /// of the pair arrives. A card that went back to offering would be the
    /// second push undoing the press that landed between them.
    #[test]
    fn a_repeat_does_not_un_accept_one() {
        let mut conversations = signed_in();
        let moved = hear(&mut conversations, invited(0, "+connect_lobby 7"));
        let (_, id) = moved.invited.expect("an invitation");
        assert!(
            conversations
                .take_the_invite(THEM, id)
                .expect("taken")
                .taken
        );
        hear(
            &mut conversations,
            invited(1_700_000_100, "+connect_lobby 7"),
        );
        assert!(
            conversations
                .with(THEM)
                .and_then(Conversation::newest_invite)
                .expect("the invitation")
                .taken
        );
    }

    /// The history carries no entry type, so an invitation fetched with a
    /// conversation arrives as an ordinary message whose body is a connect
    /// string. Recognised, it is the same card; and the message it *is* stops
    /// being drawn, or the column would show the machinery under its own card.
    #[test]
    fn an_invitation_in_the_history_is_recognised_and_not_drawn_twice() {
        let mut conversations = signed_in();
        let Some(Wanted::History { request, .. }) = conversations.open(THEM) else {
            panic!("a history");
        };
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request,
                said: Ok(vec![
                    said(1_700_000_000, 0, "are you about?", false),
                    said(1_700_000_100, 0, "+connect_lobby 109775240000000000", false),
                ]),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        assert_eq!(conversation.invites().len(), 1);
        let lines = conversation.lines();
        assert_eq!(lines.len(), 2, "the message and the card, not three lines");
        assert_eq!(lines[0].body(), "are you about?");
        assert!(lines[1].invite().is_some());
        assert_eq!(
            lines[1].invite().expect("the invitation").lobby(),
            Some(109_775_240_000_000_000)
        );

        // And this account's own connect string is not an invitation *to* it.
        let mut conversations = signed_in();
        let Some(Wanted::History { request, .. }) = conversations.open(THEM) else {
            panic!("a history");
        };
        hear(
            &mut conversations,
            Word::History {
                with: THEM,
                request,
                said: Ok(vec![said(1_700_000_100, 0, "+connect_lobby 7", true)]),
            },
        );
        assert!(conversations
            .with(THEM)
            .expect("a conversation")
            .invites()
            .is_empty());
    }

    /// What counts as a connect string, and what is somebody talking about one.
    #[test]
    fn a_connect_string_is_recognised_narrowly() {
        assert_eq!(
            connect_string("+connect_lobby 109775240000000000"),
            Some("+connect_lobby 109775240000000000")
        );
        assert_eq!(
            connect_string("  +connect 127.0.0.1:27015 "),
            Some("+connect 127.0.0.1:27015")
        );
        // A lobby is a number and nothing else.
        assert_eq!(connect_string("+connect_lobby somewhere"), None);
        // And these are people talking.
        assert_eq!(connect_string("use +connect_lobby 7 to get in"), None);
        assert_eq!(connect_string("+connect_lobby"), None);
        assert_eq!(connect_string("hello"), None);
        assert_eq!(connect_string(""), None);
    }

    /// An unstamped invitation is the newest thing in the column, not the
    /// oldest: no stamp means *now*, and 1970 is where a zero would put it.
    #[test]
    fn an_unstamped_invitation_goes_last() {
        let mut conversations = signed_in();
        hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(1_700_000_000, 0, "are you about?", false),
            },
        );
        hear(&mut conversations, invited(0, "+connect_lobby 7"));
        hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(1_700_000_500, 0, "well?", false),
            },
        );
        let conversation = conversations.with(THEM).expect("a conversation");
        let lines = conversation.lines();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[2].invite().is_some(),
            "an invitation with no stamp is the latest thing that happened"
        );

        // A stamped one stands where its stamp puts it.
        let mut conversations = signed_in();
        hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(1_700_000_000, 0, "are you about?", false),
            },
        );
        hear(
            &mut conversations,
            invited(1_700_000_100, "+connect_lobby 7"),
        );
        hear(
            &mut conversations,
            Word::Arrived {
                with: THEM,
                said: said(1_700_000_500, 0, "well?", false),
            },
        );
        let lines = conversations
            .with(THEM)
            .expect("a conversation")
            .lines()
            .iter()
            .map(|line| line.invite().is_some())
            .collect::<Vec<_>>();
        assert_eq!(lines, [false, true, false]);
    }

    /// Only the last four digits of an id are ever written down.
    #[test]
    fn an_id_is_never_logged_whole() {
        assert_eq!(short(THEM), 2);
        assert!(short(THEM) < 10_000);
    }
}
