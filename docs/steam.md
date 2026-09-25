# Steam

[Documentation](index.md) · [Project home](../README.md)

- [Steam](#steam)
- [Covers, and the picture behind them](#covers-and-the-picture-behind-them)
- [The client, kept out of sight](#the-client-kept-out-of-sight)
- [How it is signed in](#how-it-is-signed-in)
- [Whose client it is](#whose-client-it-is)
- [With no connection](#with-no-connection)
- [Pressing a game](#pressing-a-game)
- [Who is on Steam, and talking to them](#who-is-on-steam-and-talking-to-them)
- [An invitation to a game](#an-invitation-to-a-game)
- [Settings > Games > Steam](#settings--games--steam)
- [Storage: libraries, and moving games between them](#storage-libraries-and-moving-games-between-them)

## Steam

The Games column opens with one row of the shell's own: **Steam**. Signed out
it offers to sign in; signed in it says whose library it leads to, and pressing
it takes the bar to that library. Where the Steam client has a `.desktop` entry
of its own, this row takes its place — two rows called Steam, one starting a
program and one signing an account in, is something a user would have to press
to tell apart. The client's entry goes from whatever column it was filed in and
not only from Games: Valve's file declares `Network` before `Game`, so on an
ordinary machine that row sits in Internet.

The client itself is still one press away, on the menu raised over that row,
and it opens two ways. **Open Steam** raises Big Picture, Valve's own console
screen — the one face of the client a pad across the room can drive, which is
why it has the plain name. **Open Steam (Client)** raises the desktop window,
for the parts of Steam that have no Big Picture screen at all. Either one on a
machine with no client running starts one, with the request already in hand —
so what comes up is Steam's own startup, on whichever account it signs itself
in as, rather than the shell's.

Signing in is a panel, and it offers both ways Steam has:

- **Scan a code with your phone.** A QR code on screen, photographed in the
  Steam app on a phone that is already signed in. Nothing is typed, which on a
  machine driven with a thumbstick is the difference between signing in and not
  bothering. Steam rotates the code every twenty seconds or so and the panel
  follows it.
- **Type an account name and password.** The name, then the password, then
  whatever Steam Guard asks for — a code from an email, a code from the
  authenticator, or a press on the phone, which needs no field at all and so
  does not get one.

The password is encrypted, in this process, under the RSA key Steam issues for
that account name, and the plaintext never leaves the machine or reaches a
`String`. What is kept afterwards is the refresh token Steam hands back, in
`$XDG_DATA_HOME/lxb/steam.json`, readable by nobody else. The password is not
stored, ever, and there is nowhere in the client it could be: signing in again
after a reboot uses the token. This machine appears in the account's Steam
Guard settings as **LineXinBar** and can be signed out from there, or from the
menu on the Steam row.

Approval is only the first half of sign-in. LineXinBar sends that refresh token
in `CMsgClientLogon` to a quiet, persistent Steam Connection Manager session
and calls the account signed in only after Steam accepts the CM logon. Steam's
license push is then resolved through PICS into the catalogue. The token is not
an `IPlayerService/GetOwnedGames` bearer, and a temporary CM, PICS or network
failure keeps the account and last good library while LineXinBar retries or
reconnects; it does not erase a valid authorisation.

Once signed in, a **Steam** column appears immediately after Games — where
somebody who has just looked at what is installed will step next — holding the
whole library:

- **Installed games first**, alphabetically, with what they take up on the disk
  and how long the account has played them.
- **Everything else underneath**, alphabetically. A game being downloaded is
  not one that can be played, so it sits with these until it is.

## Covers, and the picture behind them

A library drawn as a column of identical Steam marks says only how many games
somebody owns. So each row is Steam's own **cover** — Valve's portrait capsule,
on a card of the same shape — and the game under the cursor puts its **hero**
picture behind the whole display, crossfading to the next one as the cursor
moves and back to the shell's own wallpaper on the way out of the library.

The picture is the wallpaper while it is up, not a layer over it: it is drawn
by the same function every pane of glass in the shell uses to work out what is
behind it, so the bar's discs refract the game and the guide's blur softens it,
exactly as they do the wallpaper it replaces. It is taken well down in
brightness, and further down the side the bar stands on — key art is painted to
be looked at on its own, and the labels have to stay readable over it.

Nothing is fetched ahead of time. The covers around each cursor and the hero of
the one row actually chosen are asked for, in that order:

1. **Valve's own cache**, `appcache/librarycache`, which a machine with the
   Steam client on it has already filled — no network, and it includes the
   capsules Valve's client generates for games that never had one.
2. **`$XDG_CACHE_HOME/lxb/steam-art`**, holding what had to be fetched.
3. **Steam's content network**, once, for anything neither cache has.

A game Steam has no picture of keeps the Steam mark on its row and leaves the
wallpaper alone, and is never asked about again.

There is a **fourth picture**, wanted in one place and fetched nowhere near the
other three: the game's own square icon, which the download card in the corner
of the guide wears and which the announcement of a finished download wears after
it. It is `common/clienticon` in the game's record, and it breaks every rule the
capsule and the hero follow. It is addressed by a bare hash rather than by a
published path, so a game whose record carries none has no icon to ask for at
all. It is served from a third host — the community images, not the store's. It
is cached in a flat `steam/games` beside the Steam root rather than in
`appcache/librarycache` with the artwork. And it is not one picture but a
Windows `.ico` holding several sizes at once, of which the largest is taken: of
the thirty-three in Valve's cache on the machine this was written against, twenty
reach 256 pixels, four hold a 512 the file's own directory *calls* a 256 — the
field is one byte wide and cannot say otherwise — and eight stop at 32. Three of
them are stored as sixteen-bit PNGs, which the `image` crate's own icon decoder
refuses outright, so the shell reads the largest entry itself and hands that one
drawing over. Where a game publishes no icon the card falls back to its cover,
and where there is neither, to the Steam mark.

The shell's own cache has a ceiling of a quarter of a gigabyte, which on any
ordinary library is several times more than every picture in it — so on most
machines nothing is ever thrown away. What the cap is really for is a machine
somebody has kept for years: a picture is filed under the path Steam publishes
it at, and that path is named after the picture's contents, so a publisher
replacing a game's cover leaves the old file behind under a name nothing will
ever ask for again. Over the cap, what goes is what has gone longest without
being *looked at* rather than what was fetched first — a picture the bar draws
is touched as it is read — so the files that go are the ones nothing has asked
for, and not the artwork of the games somebody has had longest.

The library is ordered installed-first, and the covers say which half a row is
in: **a game that is not on this disk is drawn colourless**, at the picture's
own brightness rather than a wash of it, so the games that can be played are the
only ones in colour. A download finishing takes the colour back over a third of
a second instead of between two frames — and the highlight stays on the game
while it happens. The row moves halfway up a list of hundreds as the library
re-sorts around it, so the cursor follows the game and the column is redrawn from
the same place: somebody who waited at a game's own row watches it become
playable there, rather than being left looking at whichever title closed the gap.

## The client, kept out of sight

Everything above this point happens without Valve's client: the account is
signed in over `IAuthenticationService`, the library comes from a Connection
Manager session and PICS, and what is on the disk is read from Steam's own
`appmanifest_*.acf` files. Starting and installing a game are different, and
this shell no longer pretends otherwise.

Valve's client is what plays a Steam game. It brings the Steam Linux Runtime,
the Proton the player chose, the prefix that game already has its saves in, the
overlay, the anti-cheat, and the `steamclient.so` a game's own Steamworks talks
to. There was a version of LineXinBar that did all of that itself. It worked
for a single native binary and broke on everything else, and it is gone.

What is left is the client, run as a **background process nobody sees**:

- `-silent`, so it opens no window on the way up. It governs how the client
  *starts*, not what it does afterwards: it still raises a "starting game"
  dialog and its storefront behind that, and those are kept off the screen by
  the compositor instead — see **Out of sight**, below.
- `-nofriendsui`, so nothing appears when somebody comes online.
- `-noverifyfiles`, because minutes of disk on a cold start is not why it is
  being started.
- `-nocrashdialog`, because a client that has fallen over must not put a dialog
  in front of a game.

**Whether there is a client at all** is asked of two things, and neither of
them is a directory somebody's data lives in. A native client is a `steam` on
`PATH`. The Flatpak is a *deployed commit* — `app/com.valvesoftware.Steam/current/active`,
in any installation Flatpak would look in: the user's, the system's, or one
named in `/etc/flatpak/installations.d`. It used to be asked of
`~/.var/app/com.valvesoftware.Steam`, which is the one thing about a Flatpak
that reliably outlives it, since `flatpak uninstall` keeps an application's
data unless it is asked for `--delete-data`. A machine that had Steam and
removed it therefore looked exactly like one that has it, and every row needing
a client was offered against a `flatpak run` that could only fail — silently,
because a `spawn` that succeeds says nothing about the process that exits a
moment later, and then loudly three minutes on, about a debugging port.

**A client that has never been run is still a client**, and the shell starts it
a first time rather than reporting it missing. The package installs a launcher;
the client unpacks itself into `~/.local/share/Steam` on its first start, so
until somebody has run it there is no directory to read a state out of — and
that absence is what this used to report as "there is no Steam client installed
on this machine", from the very paths that could have started one. A first
press now makes that directory, puts the marker in it and starts the client. If
that outlasts the wait, what is said is what is true: Steam is setting itself
up, which it does once, and will be ready in a few minutes. It goes on doing it
in the background, so the next press is an ordinary one.

The two kinds of client are also asked separately where their directories are.
The Flatpak runs with its home remapped into `~/.var/app/com.valvesoftware.Steam`,
so its pipe and its registry are in there beside its library, and the native
client's are in the real home. Asking the disk which of them exists — which is
what this did — pairs whichever is found first with whichever client was found,
and on a machine that has had both that is one client's log read for another
client's state.

It is started when there is Steam work to do and not before, and whether it is
running is asked of the FIFO it holds open rather than of its pid file — that
file looks like the obvious answer and is a trap, since every `steam`
invocation overwrites it with its own pid, including the one-shot helpers this
shell itself runs.

Whether it is *signed in* is read from its connection log, and the same care is
needed there. Every line it writes carries a state and an account, in that
order, and only the first of them means anything:

```text
[12:41:24] [Logged Off, 4, 0] [U:1:82105993] LogOn() called; not connected yet
[12:41:25] [Logged On, 4, 7]  [U:1:82105993] RecvMsgClientLogOnResponse() : processing complete
```

Reading the account stamp alone — which is what this did — makes a client two
seconds into starting look signed in, because it has already written the account
it is *about* to log on as. Every press then went to a client that could not
answer it. The log is appended to across runs as well, so a client shut down an
hour ago still has the last word in the file; only the current run counts, and
`Client version:` is the line that starts one.

## How it is signed in

A client that has to ask who is signing in puts up its own login window, which
is the one thing this must never do. So it is signed in before it is ever
started, with the credential this session already holds.

That credential is not borrowed from anywhere. LineXinBar asks Steam for a
*Steam client* session, so what it gets is exactly the kind of token the client
would have got for itself; the account sees one device, named LineXinBar, and
revoking it there ends both halves at once.

Valve's client is a web application — everything above `steamclient.so` is
JavaScript in an embedded Chromium — and its login screen finishes the same
authentication this shell runs by calling

```js
SteamClient.Auth.SetLoginToken(strRefreshToken, strAccountName)
```

so that is the call LineXinBar makes. From cold, the whole of it takes about
four seconds and the client's own log says `RecvMsgClientLogOnResponse : 'OK'`.

Reaching that interface needs the client to be told to expose it, which is one
file in the Steam directory — `.cef-enable-remote-debugging`. The shell makes
it, the first time it has a reason to sign the client in; there is nothing to
type and nothing to set up. There is no command line switch that says the same
thing, so the file is the whole of the mechanism.

The client reads that file only as it starts, and everything else follows from
that. **A client this session starts is started exposed**, whatever it was
started for — the marker goes up, the client comes up against it, and the marker
comes straight back down. So a Steam you start yourself next week comes up
exposing nothing, and the port belongs to the client the shell is already
driving and keeping out of sight.

That promise has one way of failing and it is now closed. The marker is made and
taken back inside a single call, so a session killed between the two — a crash,
a power cut, a development run stopped with `Ctrl-C` — leaves it on the disk, and
nothing would ever have removed it: every later attempt finds it already there
and correctly decides it is not the shell's to touch. The machine this was
written on had one a fortnight old, and had been exposing its client on every
start since. The shell now writes down, in its own state directory, that a
marker is one it made, and takes that one back the next time a session starts.
A marker somebody made themselves has no note beside it and is left exactly
where it is — Valve documents the file, and a developer may well want one.

Refusing to remove it is right and it is not the whole answer, because a marker
made before any of this was written has no note either, and so is nobody's to
remove — forever, silently. So the Steam diagnostics panel now has a line for
it. **Debugging interface** reads `closed`, or `open, this session's` while a
sign-in is going on, or `shut, but marked to open again`, or `open to anything
you run` — the exact truth of it, since this is a loopback socket with no
authentication and every game, Flatpak and script started as that user can drive
the client through it. Two separate facts, because they come apart: the marker
decides what a client becomes as it starts, and the port is what the client
already up is doing now. A marker deleted under a running client leaves it
`open until Steam restarts`, and the panel says that rather than claiming the
job is done.

Beside that line is **Close the interface**, and it appears only when there is a
marker to take away that this shell did not make. The rule has not moved: the
shell will not decide on its own to turn off something somebody else turned on.
Being asked to is not the shell deciding. A session that starts with the
interface open also says so once in the log, which is the thing that was missing
— the fortnight above was not found by reading the code.

It used to be lazier than that, opening the interface only when something
reached for it, and that is what broke installing. The first install of a
session found a client that was already up and exposing nothing, and the only
way to change that is to stop the client and start it again, because the marker
is read once on the way up. Caught on a real machine: the client was asked to
shut down, did not let go of its pipe within thirty seconds, and the press
failed; pressing the row a second time worked, because by then the client was
stopped and could be started fresh. One file that exists for a second is a
better trade than stopping somebody's Steam to open a port.

A client that was *already* running when the session found it is the one case
still answered by a restart, and only when the interface is genuinely needed.

**Out of sight.** The client is a program this shell drives rather than
presents, so the compositor is asked not to show it — `lxb_shell_v1.keep_out_of_sight`,
by the two names its windows carry, `steam` and `steamwebhelper`. Its windows
are not drawn, not listed as being on any display, never given the keyboard, and
the pointer goes through them; the client itself is mapped, configured and
drawing exactly as it would be, and is simply never looked at.

Hiding is the shell's to give back, and it has a reason attached so that it can
be. Sight is given for a hand-over — Open Steam, Verify, Install with Steam —
and taken back when the window the user asked for **has gone**, or when it
**never came** within thirty seconds. Without the second rule a `steam://`
request the client did not understand left the session revealed for the rest of
its life, waiting on a window nobody ever saw.

**And a hidden window that has stopped being work.** The client does not only
show windows while it is busy; it also stops and asks things — a sign-in it
wants doing again, an agreement, a licence key, a parental code, a conflict
between two saves. Hidden, every one of those is a session that has quietly
stopped, with a bar on the screen and nothing wrong with it and nothing working.

So the compositor says what it is keeping off the screen — `unseen_window` — and
the shell, which knows what it asked the client for and what it is still waiting
on, can let a single window through: `let_this_window_be_seen`, one window, the
rest of the client still hidden.

The rule reads **no wording of Valve's**, deliberately: Steam is translated into
every language it ships in, and a rule built on the English for "Launching…"
works in one country. It asks three things instead. The shell must not be
showing something of its own about Steam — a loading screen owns the display it
is on and has its own patience, and the client's own window behind it is that
same wait said twice. It must not be the storefront, which is the one window the
client has whenever it is running at all, and which is matched on the client's
own name because that is a brand and not a sentence. And it must have stood for
longer than anything this shell asks the client to do, which is where the number
comes from rather than from taste: the install wizard's own patience is 45
seconds, so this is a minute.

That is a long time to leave somebody looking at a session that has stopped, and
it is still the whole of the improvement — before it, the wait was the rest of
the session.

## Whose client it is

One machine has one Steam pipe in one home directory, and every session on the
machine shares it. So "is a client running" and "is a client *ours*" are two
questions, and the shell asks both before it uses one — because the failures
either way round are invisible from here.

**Which session it is drawing into** is read out of the running client's own
environment: where a client draws is fixed when it starts, and no URL handed to
it afterwards can move it. A client belonging to a desktop the user left running
behind this one answers the pipe, takes `steam://rungameid/…` and starts the
game perfectly — onto that desktop. The shell's loading screen then waits out
its patience for a window that was never coming here, the game's own log records
a clean launch, and nothing anywhere says what went wrong.

**Which account it is signed in as** is read from the same connection log the
signed-in state comes from: the stamp is `[U:1:<account>]`, which is the low
half of a SteamID, and it is compared against the account this session actually
holds a credential for. A household's second account passes every other test
there is and then installs into, verifies and plays out of the wrong library.

Neither is answered by taking the client. Moving it means stopping it and
starting it again here, which ends whatever it was doing — downloads included —
and signing it in as this account signs the other one out. Both are somebody
else's, so both are a **question**: the press is refused, nothing is touched,
and the panel offers **Move Steam Here** beside **Not Now**. Only the answer to
that question may stop a client this session did not start.

The same rule covers signing out. Signing out of LineXinBar stops Valve's client
only when it is this session's; a Steam running on the desktop next door is left
alone, with its download.

**Unsure counts as ours**, in both. Every way of not finding out — no process
holding the pipe, an environment this shell may not read, a log it cannot open —
answers "ours", because the two ways of being wrong are not equal: treating our
own client as foreign puts a question in front of somebody for no reason, and
treating a foreign one as ours takes their Steam away. The destructive answer is
only ever given on evidence.

While it is exposed the client listens on `127.0.0.1:8080`, and any program
running as you can drive its interface through that port. Nothing is opened to
the network.

**What this session drove through it is written down.** Every operation and what
it came to — a wake, a sign-in, an install, a stop, a removal, a `steam:` URL
handed over — goes into `$XDG_STATE_HOME/lxb/steam-actions.log`, oldest
half dropped when it grows past 64 KiB, and away entirely when the account signs
out. Two readers: somebody whose game will not install, whose shell said a
sentence about it a minute ago and has since gone back to the bar; and anybody
asking what a session did to Steam on a machine where that port is open.

Nothing secret is in it, and that is enforced where it is written rather than
promised by whatever writes it. The danger is specific: the expression that
signs the client in *contains the refresh token*, and an expression that throws
comes back with Chromium's own description of it, quoting the source. So every
string is passed through a redaction that replaces any run long enough and
credential-shaped enough to be a token. An app id is six digits and survives; a
JWT does not.

The last few lines of it are on **Steam diagnostics**, a panel on the Steam row's
own menu: which client is being driven and out of which directory, whether Steam
is answering, how old the column is, what is moving on or off the disk, what the
picture cache takes, and what was asked of the client lately. It carries the one
button in the shell that empties that cache, beside the number it is about. It is offered in every state, which is the point
— nobody signed in, no client on the machine, Steam not answering are exactly
the states where the other rows are the ones that have gone. The account's number
appears as its last four digits, which tells one household account from another
and identifies nobody.

**`SteamClient` is Valve's own internals and is promised to nobody**, so a
client update may rename or remove any of it — and the shape of that failure is
a press that silently does nothing: a wizard opened with nobody listening, its
whole patience spent, and Valve's unhelpful wording at the end of it. So the
client is asked what it still has before any install or removal is driven, and a
method that has gone is answered the way a game with a product key to type is:
Steam's own window, offered plainly, rather than a failure the user could do
nothing about.

The obvious alternative was tried first. The client keeps its credential in
`local.vdf` under `ConnectCache`, and on Linux the value there is the token in
the clear — Valve's own strings give that away, since the client recognises a
modern token by sniffing for the base64 of `{ "typ": "JWT",`. The key that
entry is filed under is another matter: ten derivations were tried against a
real token on a real client and every one produced the same line in its log,
`cached creds not available`. Writing to an unpublished format is a guess that
breaks when Valve changes it. Calling the method its own login screen calls is
not.

## With no connection

**Installed games play with no network at all**, which is what Valve's client
calls Offline Mode and what LineXinBar now reaches for on its own.

The account is signed in before Steam is asked about it: the credential is on
this machine, so the column comes up out of the remembered catalogue and the
manifests on the disk, dated rather than passed off as current: the Steam row
wears what Steam said and how old the column is, "· library from an hour ago".
Pressing an installed game then starts
Valve's client the way it always does, and the difference is what the client is
asked for. A session that has *tried* Steam and failed asks it for Offline Mode;
a session that is still connecting does not, because a boot that was going to
succeed spends a second or two there and a press made in that second must not
leave somebody's Steam coming up offline for the rest of the week.

Offline Mode lives in the client's own list of accounts, `loginusers.vdf`, as
`WantsOfflineMode` on one account's entry. Writing it before the client starts
is the whole of how a machine with no keyboard enters the mode: the client reads
it as it comes up, logs on to this machine alone in about a second, and never
touches the network. The two obvious alternatives were tried first and are
worse — `steam://gooffline` and `steam://goonline` are real verbs in the
client's URL table and are silent no-ops on the current build, and
`SteamClient.User.StartOffline` means opening the client's debugging port and
restarting a client that came up without one, which is a minute of somebody's
loading screen to write a field the shell can write in a millisecond.

A client in that mode never says `Logged On` in its connection log and never
will, so "is it signed in" is not one question but two: whose account the
running client stamps its log with, and whether that account asked for the mode.
Either alone is a client that would open somebody else's library. The pair is
also what stops a client that has *just been started* being taken for one that
is ready — the log is appended to across runs, so for the second before a new
client writes its own first line the tail still belongs to the one before it,
and a run that ended with `Log session ended` speaks for nobody.

The mode is turned off again only where the shell turned it on. A marker in the
shell's own cache holds the account it asked for, and Steam answering — a CM
logon, on a later boot with the network back — is what spends it: the field is
cleared, and the next client comes up on Steam. Somebody who chose Go Offline in
Valve's own menu keeps it, because that is a decision the shell did not make.

What offline costs is Valve's, not the shell's, and it is worth saying plainly.
Nothing can be installed — the client refuses, and so does the shell, which says
so on the press rather than opening a wizard that cannot fetch anything. The
library is a memory until Steam answers again. And only games that are fully up
to date will start; one with an update waiting is refused by the client itself.

## Pressing a game

**Pressing an installed game plays it.** The splash grows out of the tile that
was pressed, exactly as it does for every other row on the bar, and stays until
the game's window arrives underneath it. What happens in between is the shell's
business and is not narrated: the client may have to be started and signed in,
which from cold is most of a minute, and a loading screen that explained its own
plumbing would draw attention to the thing it exists to hide.

That splash follows three rules of its own. It waits on **two clocks rather
than one**, because it is two waits: two and a half minutes for the client to
come up and sign in, and then a fresh minute for the game itself, measured from
the moment the client is actually asked. That is longer than the twenty seconds
an ordinary application gets, because the client may update the game, build a
Proton prefix on its first run, unpack a shader cache or show an anti-cheat
installer first, and none of that is failure. It never concludes the game has
died: the `steam steam://rungameid/…` that carries the request exits within
milliseconds of being started, and the game is the client's child rather than
this shell's, so only the window counts. A game that never appears is said so
plainly rather than passed over — the splash has just spent minutes promising
that something was happening — and the panel that says so offers to try again
or to open Steam, because "OK" is not an answer to a game that did not start.

**A launch can also stop rather than fail**, and the two look identical from
outside. Valve's client walks a launch as a job of its own with a task it is on,
and some of those tasks are not steps but questions: a save in the cloud that
disagrees with the one on the disk, an agreement, a launch option, a parental
code. It stops on them until somebody answers — in a window this shell is
holding off the screen, because a launch is exactly when Steam's own windows are
least wanted. Waited out, that is a loading screen that runs its whole patience
and then a panel saying the game did not start, with the answer sitting one
click away in a client nobody can see. It was reported from use, on a game whose
save the cloud had not caught up with.

So the client is asked. `GetActiveGameActions` names the launch it is running
and carries one boolean, `bWaitingForUI`, which is the client's own answer to
"have I stopped and do I need a person" — no wording to recognise, nothing that
ships in a different language, nothing inferred from how long a step has taken.
It is polled every two seconds for as long as a press is waiting, and only then.

When it says yes, the loading screen comes down and **the question is asked
again on the shell's own panel** — the game at its head, Steam's own sentences
under it, and Steam's own answers as rows a thumbstick can reach. Choosing one
carries it back to the client and the loading screen goes back up; choosing the
refusal ends the launch and nothing starts.

**Valve owns the words and the shell owns the buttons**, and that split is the
whole of it. Every line and every label is fetched from the client's own
localisation table by token, so what is on the screen is the sentence Steam
would have shown, in the language it is running in; and the strings that carry
each answer — `IgnorePendingCloudSessions` for a save the cloud has not caught
up with, `KickOtherSession` for an account playing somewhere else — are
transcribed from the client's own dispatch rather than invented. What the shell
supplies is a panel that can be pressed, because Valve's own dialog for these is
a desktop one and a person holding a controller cannot reach it. Its own
headings are left out: they are window titles — "Error - Steam" — and the head
of the panel is the game, as it is for every other panel about a game.

Not every question, and the ones left out are left out on purpose. A product key
has to be copied down; a conflict between two saves is a choice Valve shows with
the date of each, and asking somebody to pick one blind is worse than asking them
to reach for a mouse once. An agreement asked *at launch* — which only happens
when a publisher rewrote its terms after the game was installed — is left to
Valve's window too: the client puts up a dialog of its own for it the moment it
asks, hidden here, and that dialog cannot be closed from outside, so answering on
the shell's panel would leave Valve's standing behind for the next time the
client is seen. Those still give the client sight, the older answer, where
somebody with a pointer can deal with them. An agreement asked *before an
install*, which is where nearly all of them are, is the shell's — see below.

This needs the client's own interface, which the shell opens on any client it
starts itself. A client that was already running when the session came up is not
exposing one, and there the wait is what it always was.

And **it can take the hand-over back.** Handing the screen to the first new
window is a bet, and the bet is sometimes lost: an X11 toolkit builds a window,
throws it away and builds the one it meant; a game swaps its window for a
fullscreen one. For the few seconds that takes, the display is the bare bar
with nothing on it — a press that reads as a game which failed to start, to
anybody who does not know to keep waiting. So the splash goes on watching after
it has faded, drawing nothing, and comes back if what it handed over to turns
out to have gone. Only for a display with nothing left on it: a game that
merely changed which window it was showing has not gone anywhere.

**Which window the game turned out to be** is asked of the window's own process
before it is asked of the clock. A window says nothing about who started it —
what it announces is a class, and a Proton title's is whatever its binary was
called, routinely `x86_64` for a whole shelf of them — so the shell used to
answer entirely by timing: the window that was not there when the loading screen
went up is the game. That is right until two things appear at once, and during a
game launch two things routinely do. Valve's client raises its own windows on
its own schedule, an anti-cheat installer comes and goes, and everything else on
the machine goes on running; each of them was recorded as the game, and from then
on the guide offered to close the game and closed something else.

Steam runs a game it starts with the app id in its environment, so the process
behind a window can simply be asked — the compositor says which process drew each
window, over `output_window_pid`, and the shell reads it out of `/proc`. It
settles both directions: a window that says it is a game is one, whoever started
it, so a game launched from Steam's own window is nameable too, which timing
could never do; and a window that says it is a *different* game is one this
launch may not claim. Valve's own client and any application this machine has
installed are refused outright. Only where nothing says anything — the ordinary
case, and most windows — does the timing decide, as it always did.

There is a second source behind the first, for the window whose process nobody
knows: an X11 client that set no `_NET_WM_PID`, or one whose socket carried no
credentials. Valve starts a title under a class of its own making,
`steam_app_<id>`, and that names the game as plainly as the environment does.
Both were read off a live client rather than assumed — `steam://rungameid/…` for
one installed game raised a window classed `steam_app_3812600` while every
process from the reaper down to the game carried `SteamAppId=3812600`.

**A game the account owns and the machine has not got is fetched by pressing
it.** The press offers rather than starts, because a press meaning "I want
this" and a press meaning "and spend forty gigabytes on it now" are the same
press and only one of them can be taken back.

The client's own installer does it, driven through the same interface — and
driven from the client's own events rather than by calling three methods and
hoping. That flow is a state machine the client walks at its own pace, and five
of its states are questions for the person rather than steps for the program;
it sits in whichever one it reaches until somebody answers. So the shell
subscribes to `RegisterForShowInstallWizard` and answers each state as it
arrives: no shortcuts at `ShowConfig` (this shell *is* the menu, and the game
is already a row on it), then continue, then wait to be told the download is
queued. There is no wizard window, because with `-silent` there is no window
for one to be drawn in.

This is where installing was broken. The old version returned `{ result: 1 }`
— a constant it wrote into its own answer, never read back from the client —
so a flow that had stopped to ask a question came back looking exactly like one
that had queued a download. The row said "Installing…" for the rest of the
session with nothing coming down and nothing said.

**And it waits, before it starts, for the client to know what the game is.** A
client that has signed on does not yet know what the account owns: the two
finish seconds apart, and in between, the wizard is asked to install a game it
has never heard of. It walks two states and fails, with an empty `rgApps`, a
required size of zero, and an error number that is different every time — 29
once, 6 another. That was the first install of every session, which is why the
same press worked the second time: by then the client knew. So the shell waits
for `appStore.GetAppOverviewByAppID` to answer for the game before it opens the
wizard at all. Measured from a stopped client on the machine this was found on:
the wait was 1.3 seconds, and the press reached "fetching" 5.6 seconds after it
was made. A question that cannot be asked counts as answered, so a renamed
store costs the press nothing rather than twenty seconds.

The wizard has its own reader for what it says, too. A call the client refuses
answers with an `EResult`; the wizard is a state machine, and the number it
carries when it fails is an `EAppUpdateError` and not an `EResult` at all.
Running both through one function is how a failed download came to be reported
on screen as *"Steam would not take the credential"* — a sentence about a
password, printed over a game.

**Which library a game goes into is asked on the shell's own panel**, on a
machine with more than one. Valve's wizard stops at `ShowConfig` whenever there
is more than one library and the game may be moved (`bCanChangeInstallFolder`)
— its own dialog would show a folder list there — and the shell used to go on
with the client's default, which was answering the question for the person.
Now, with Settings > Games > Steam > Storage > **Install games to** on **Ask every time**
(the default), the shell reads the libraries Valve's list reads
(`SteamClient.InstallFolder.GetInstallFolders`: path, the name given in Steam,
free space, whether it is the default) and the room the game needs
(`nDiskSpaceRequired`), cancels the wizard, and puts up a panel: the game's
name, "Choose where to install it. It needs 46 GB.", a button per library
("Games · 412 GB free") and **Not now**. A library the game will not fit on is a
greyed-out button saying so, by Valve's own test (the room needed is less than
the room free). The cursor starts on Steam's default where the game fits there.
Pressing one opens the wizard again and makes the call Valve's folder list
makes, `Installs.SetInstallFolder`, before going on. A library is named by the
name somebody gave it in Steam, else by the drive it is on as Settings >
Storage names that drive.

With a library chosen in the settings instead, nothing is asked: the wizard is
pointed at it, and it is made Steam's own default too
(`InstallFolder.SetDefaultInstallFolder`, the call behind Steam's "Make
default"), so a game started from Steam's window lands in the same place. That
is told to a running client on the settings press, without ever starting one,
and to any client on the next install into it. A chosen library that is not
there — a drive unplugged today — or that has not the room is asked about,
with the reason at the top of the panel, rather than quietly swapped for
Steam's default. Choosing a library for one game on the panel is that game's
answer only. The choice rides through an agreement: a game asked where, then
asked to accept terms, goes where it was sent once they are accepted.

A client that has lost `GetInstallFolders` or `SetInstallFolder` installs where
it always did — into its own default — rather than refusing the game. A
machine with one library is never asked anything and has no row for it.

`cargo run -p lxb-steam --example probe-install -- <app id>` reports the
choice and fetches nothing; `--to <library>` fetches into one.
`--debug-actions library` photographs the panel with three invented libraries,
one of them too small, and installs nothing.

Which build comes down is the client's decision — this system, this account's
licences, the depots the game is actually made of. A shell that passed its own
opinion in would be a second implementation of that decision, able only to be
wrong in ways Steam's is not.

**Two games in five stop the flow to ask something**, and nearly always it is an
agreement. Of fifteen titles taken off one real account, six had one — Black
Mesa among them, which used to sit in `ShowEULAs` with no window, no download and
no error. Garry's Mod has two: Facepunch's terms of service and its privacy
policy.

**An agreement is asked on the shell's own panel.** When the wizard stops on one,
the shell asks the client for its list (`SteamClient.Apps.LoadEula`) — each
agreement's name, its edition and where its words are — cancels the wizard, and
fetches the words from the store the way Valve's own dialog does (the address
with `eulaLang` and `json=1` on the end; no credentials, and English where the
publisher wrote nothing in the shell's language). The panel is the game's name,
one sentence, the agreement in a reading well scrolled a screen at a time with
Left and Right — the terminal frame's well, in the shell's own face — and
**Accept and install** above **Not now**. Several agreements are shown one after
another, as Valve shows them, and only the last one's button installs.

Nothing is accepted until that last press, and then it is recorded with
`MarkEulaAccepted` — the call Valve's own Accept button makes, with the same three
arguments — before the wizard is opened again, which then finds the agreements
accepted and goes straight through. Recorded first rather than answered inside a
parked wizard, because the client's own interface reacts to `ShowEULAs` too: it
builds a hidden dialog of its own that cancels the install when it closes unless
*its* workflow finished. An agreement whose words could not be read is never
offered for accepting — the panel says so and offers to try again — and a client
that asks again for agreements it has just been told were accepted is answered
the older way rather than with the same panel twice.

The rest — a product key, a password, a disc, an account to sign up for — are
reported rather than answered: the panel says what the game is waiting for and
offers **Install with Steam**, which hands the whole install to Steam's own
window. The other three games in five never see Steam at all.

**How far it has got is asked of Valve's client**, over the same loopback
interface a shell-driven install is already being driven through, and this is
one of the few places in this integration where the disk turned out not to be
enough. `BytesDownloaded` in the game's own `appmanifest_*.acf` — which is where
every other fact about an installed game comes from, and where this used to come
from — is written so rarely that it says nothing. Measured through a whole
install of a 626 MB game on real hardware: it read **64 bytes** while all 626 MB
arrived, and on an earlier run it read nought for six of the nine seconds and
briefly nought *again* halfway through. Over those same nine seconds the
client's own account went 4%, 20%, 47%, 56%, 71%, 84%, 100%, with a rate and a
time remaining beside each. Steam's own window is drawn from that, which is why
it is the one that moves.

`SteamClient.Downloads` offers a registration and no getter, so the shell leaves
a callback in the client's page that writes each overview down, and every poll
reads what it last wrote. What comes back is the app id — there is one download
at a time, and a row must not count somebody else's — the percentage, the
network rate, and how much longer the client thinks it will be. The poll is
given two seconds of patience rather than the twenty a command gets: a late
answer to this is worth nothing, the next one is already due, and a client that
has stopped answering must not stall the worker reading the disk beside it.

**The manifest is still read, and still matters**: it carries the *size*, it is
what a client with its interface shut leaves behind, and it is the only account
of a download somebody started in Steam's own window. So the row prefers the
client and falls back to the file — and where the client knows the percentage
before the file names a size at all, which is most of the first second, the row
says the half it has rather than "of 0 B".

**The row says it three ways: a percentage, a rate, and a bar under them.** A percentage says
how far; a bar says how far *of what is left*, which is the question somebody
standing and waiting is actually asking. It is drawn as a reading rather than as
a control — the same groove and fill the volume and brightness bars use, and
deliberately without their handle, because a dot on the end of the fill says the
value is somewhere a thumb can move it to and there is nothing here anybody can
set. It does not glide towards its value either, though nearly everything else
in this shell does: the number beside it is the truth about a file on a disk,
and a bar easing towards that number would be showing a percentage the download
has not reached, on the same row as the words saying what it really is.

Nothing is drawn until there is something to draw, and that is two rules rather
than one. **The two byte counts only belong to the standings that are actually a
download.** On the rest they are leftovers from whatever Steam last did with the
app, and reading them anyway put a bar under every installed game on the machine
this was written on: mostly full, because the last download finished, and
sometimes empty, because a pending update had set a size and fetched none of it.
A game being verified read "Checking files · 100%" off numbers belonging to a
download that had ended days earlier. And **nothing having arrived is not nought
per cent**: it is the absence of a reading, the shell already has a word for that
state, and a row printing "0%" beside an empty groove was the whole of what a
short install looked like. At the other end the fill is never thinner than the
bar is thick: three thousandths of a hundred-gigabyte game is half a pixel,
which reads as nothing at all.

**And the disk is looked at more often while something is moving** — every two
seconds rather than every ten, which is about the rate Valve's client rewrites
the manifest. Ten seconds is the right rate for a question nobody is watching;
it is the wrong one for a bar, which would step so rarely that it read as a
download that had stopped — and this shell says *that* separately, in words,
which is the whole reason the two must not look alike.

The RetroArch row directly above draws the same bar for the same reason. It is
the neighbour of these rows in the same column, it already said a percentage in
words, and one kind of waiting drawn two ways would be two kinds of waiting.

**And the same fact is in the corner of the guide**, because the row is on a
screen somebody has usually left. Open the guide while anything is coming down
and a card stands in the far corner from the menu column, wearing the game's own
icon, saying what is arriving, with the bar and the percentage under it. It is a
reading and not a control: it takes no selection, answers no press, and there is
nothing on it to aim at — what a person *does* about a download is on the game's
own row and in its menu, and this is the corner of the screen saying that
something is happening while they are somewhere else doing something else. It
comes in from off the right edge on the flight an announcement arrives on, and
leaves the same way from wherever it had got to, so a download that finishes
while its card is still arriving is never seen to snap.

It is drawn **only while the menu is open**. A card standing over a running game
would be the shell putting news on somebody's screen that they did not ask for
and cannot dismiss; the guide is a place they have deliberately come to, and it
is the one place where a corner is free. Over the menu it is allowed to stand on
a window card, which is what the concept it was drawn from shows, and it never
reaches the column: it is a panel's width pinned to the opposite corner.

It keeps that corner when the machine is updating itself as well: the update's
own card — the same card, wearing the update mark, see
[Updates](updates.md#using-the-page) — stands on top of this one rather than in
its place, and comes down into the corner only once this one has finished
leaving.

The one download, not a list of them. Valve's client downloads one game at a
time and the rest of a queue is waiting rather than arriving, so the card shows
what is actually moving — a game this session pressed install on first, because
that half knows the client's own account of it and answers before a manifest
exists, and the disk otherwise, so a download begun in Steam's own window an
hour ago has a card too. The words are the row's words: *Downloading* for a
first copy, *Updating* for bytes arriving over one that is already here.

**When it finishes, it is announced — quietly.** The card goes and a bubble
takes its place in the opposite corner, wearing the same icon, saying the
download has finished. It is filed behind the bell like anything else, it lights
the unread mark, and do-not-disturb keeps it out of the corner on exactly the
terms it keeps everything else out. What it does not do is make a sound: a
download finishing is news somebody is glad of and never news they have to act
on, and it lands at whatever hour the line happened to finish, quite possibly
over a game or a film.

A download *stopping* is not a finish and is not announced. Steam pausing one
takes the card away too, and so does a copy turning out to need repairing — so
the game has to be on the disk and ready to play before a word is said, and the
row says which of the other things happened in its own words, which is a better
answer than a bubble about it.

**The first seconds of an install are not a pause, and this shell used to say
they were.** Valve's client opens a fresh install with no working bit set at all
— `Update Required,` and then `Update Required,Update Queued,`, read off the
client's own `content_log.txt` — and a manifest with nothing in flight was taken
for a download somebody had stopped. That was not only a wrong word on a row.
A pause is a state the shell *stops watching* on, so the shell let go of the
install it had just been asked to make, and every number on that row afterwards
came off the disk with nobody following it. What tells the two apart is whether
anything has arrived, and Steam says so itself with `Update Queued`.

The bits behind those words were read off the machine rather than taken from the
tables that circulate for them, and two of those tables are wrong for this
client: `content_log.txt` prints every change twice, once in words and once as a
number, so the pairs decode each other. `8` is `Update Queued`, not `Encrypted`;
`16` is `Update Optional`, not `Locked`; `8192` is `App Running`.

A pause is two states rather than one, and it matters which. A download stopped
part way is not something to press; an *update* stopped part way sits over a
copy that is whole, and that game is playable — the same distinction the shell
already draws between a download and an update while the bytes are moving. It
was found on a real machine: a game somebody had been playing that afternoon,
with an update Steam had stopped, whose row said "Download paused · 100%", had
no **Play** on it, and offered **Stop and Delete** against a hundred gigabytes
nobody had asked to lose.

There is exactly one thing that file cannot say, and the shell keeps its own
clock for it. Every state a download *stops* in is written down — paused,
paused over a playable copy, needing repair, being removed — and all of them are
read off the disk and said in the row's own words. A client that goes on claiming to download while nothing
arrives writes nothing at all, so the row counted the same percentage until the
session was restarted. An hour without a single byte now adds **— not moving** to
the row and puts Steam's own downloads list on its menu, which is where whatever
is wrong with it can be looked at. Nothing is cancelled and nothing is deleted:
a byte arriving takes the words back. And the end of a large install — where
`BytesDownloaded` stops moving because the client has everything and is unpacking
it — is deliberately not this, or the rule would be wrong on every big game
anybody ever installed.

**Removing a game is the shell's own press.** It is `OpenUninstallWizard` with
the flag that means "already asked", so nothing of Steam's appears; the asking
moves into the shell's own panel, which is where the size it frees is already
written. It waits for the same thing an install waits for, and for the same
reason: this press names a game, and the row it was made on was read off the
disk, which is ready long before the client is. The `steam://uninstall/…` URL this used to hand over was the same
removal with Steam's confirmation window over the top of the bar, which is the
one thing the integration exists to avoid. Stopping a download is the same call
— that is what makes the panel's promise that nothing is left behind true,
where taking the app off the download list left a stopped 140 MB install as
368 MB under `steamapps/downloading` and a row reading "Downloading" for ever.

**Verifying** stays an explicit *with Steam* action on the game's menu, and is
named for it because it does raise the client's own window: it runs for minutes
and Steam's is the only account of how it is going. Every row over a game needs
the client, so a machine without one offers none of them rather than rows that
can only refuse.

What is installed is read from the same files the client keeps —
`steamapps/libraryfolders.vdf` and the `appmanifest_*.acf` beside each game, in
every library on the machine — so the two agree by construction. A game
installed in Steam an hour ago is already marked as installed the first time
this shell is signed in, and one that finishes downloading moves to the top of
the column within ten seconds. Steam's own runtimes are left out of it: Proton
and the Linux runtimes declare themselves with a `toolmanifest.vdf`, and nobody
has ever wanted to press one.

Pass `--no-steam` to leave the whole of this out of a session: the Games column
loses its row, no stored session is read, and nothing in the process talks to
Steam. For a machine where somebody else's account is signed in, and for a
session that should make no network connections at all — which, with this off,
is every one of them.

## Who is on Steam, and talking to them

**X** on a pad, or **Shift** on a keyboard, brings in a column down the right of
the screen: the account at the head of it, and everybody it knows under it, in
three bands — in a game, around, and not here. It is the guide's own sidebar
mirrored into the other edge, and it takes every direction while it is up. The
line under the account's nickname is a button: it puts up Valve's own four
statuses, and choosing one tells this session, tells Valve's client where one is
running, and is remembered until a client wears it.

Pressing somebody opens their conversation. The panel turns rather than being
replaced — the head cross-fades from the account to them in place, and the list
steps aside for a column of what has been said — so what arrives is the same
panel showing its other face. Messages this account wrote are on the right in
the accent; theirs are on the left in glass; a bubble is as wide as its words
and never wider than four fifths of the column, which is what says which side a
message is on without a label.

**Up walks back through what was said**, and the message the light stops on is
lit the way a row of the list is — a glow behind it, a pale ring around it and
the bubble itself brought up — because a light somebody has to look for is not
one. Beside it stands the one thing a column of bubbles never says: **when it
was said**, on the clock the session is set to, with the day as well when it was
not today and there is room beside the bubble for both. Only under the light,
and never at the cost of a line: a time against every message would be a second
column of figures down a panel 280 points wide, and one that reserved room would
shift every message below it each time the light stepped.

Nothing can be done to a message that has already arrived — Steam offers no act
on one — so **the press means what it means on the field: write**. That is also
the one press back to the field from twenty messages up, which is what the walk
had been missing: the row had named Back and nothing else, and Back leaves the
conversation. A message that did not go is the exception, and the only one: it
offers **Send again**, and that is the whole of what it offers — a message
somebody wrote is not something this shell throws away, so there is no way to
dismiss one and it stands in the column until it goes. One that is merely still
on its way offers nothing at all, because Steam has not refused it and there is
nothing yet to retry.

All of it rides the CM session the roster already comes over. There is no second
sign-in, no second connection, and nothing here goes anywhere near Valve's
client: a conversation is `FriendMessages.SendMessage`, `GetRecentMessages` and
the `IncomingMessage` push, on a socket that is open anyway. What it costs is
one field in the logon — `chat_mode = 2`, which is the only opt-in there is for
live messages, and without which Steam sends this session nothing that was said
to it.

**Nothing is written to a disk.** Not the messages, not what has been typed and
not sent, not who has unread ones. Steam keeps the history and hands it back for
the asking, and a copy of somebody's private conversation on this machine is a
thing to be designed and agreed to rather than a side effect of drawing it. What
is on the panel is what this session has been told since it signed in.

**Back comes off one layer at a time**, which on this panel is three: the field
being typed into and the keyboard over it — what was typed stays, because
nothing has been sent — then the conversation, back to the list, and then the
list, back to whatever raised it. The button that raised the panel puts the
whole thing away from anywhere in it, with no exception at all.

A message that arrives for a conversation nobody has open raises the same
notification any program's does — the sender's face, their name, and what they
said, over two lines — and puts a count on their row. **The words are withheld
while every screen is resting**: a display the compositor has taken to black
under the OLED rule is a display nobody at this machine is watching, and one
that arrives then says only that it did. Opening the conversation marks it
read.

**And that announcement is a way back into the conversation.** Pressing its row
in the notification list opens the panel on whoever wrote, exactly as pressing a
program's own announcement goes to the program — it is the one row in that list
that is about somewhere, and a press that merely cleared it would throw away the
only thing on the screen pointing at what was said. The row's own **X** is still
there for being rid of it without going anywhere. Reading a conversation takes
*every* announcement about it off the list and out of the corner, whichever way
it was opened, so three messages from one person leave nothing behind after the
first press.

## An invitation to a game

A friend in a game can ask this account to join them, and the invitation lands
in their conversation as a **card**: the game's name, a line saying they asked,
and the button that takes it up. **Y** on a controller, the **Menu key** on a
keyboard — the top face button, which is Options everywhere in this shell and
which on a line of a conversation means nothing at all unless that line is a
message that would not go. It was X for a day, and the user asked for it to
move: X raises this panel and puts it away, so a button that closed the panel
everywhere but here, where it started a game instead, is the worst kind of
near-miss.

That button has nothing else to be in a conversation. Options gave up on a
message that would not go for a day, and the user had it taken out rather than
leave two acts sharing a button — which is also why nothing here has to ask
where the light is standing. The legend names Accept first while an invitation
is waiting, wherever else the light is, and the card names it again on itself.
`A` on the card does the same thing.

Accepting is one press and it is the whole of the act: Valve's client is woken
if it is not already running, the game starts, and it joins what the invitation
was to. That is `steam://joinlobby/<app>/<lobby>/<who>` where the invitation is
to a Steamworks lobby, which is the form the client knows how to finish whether
or not the game is already running; anything else a game defines its own
connect string for is started with it on the command line. **Pressing the
announcement does the same thing** — it is the one row in the notification list
whose press starts a game, and it is there for the case the whole feature is
about: somebody is in the middle of something else when they are asked.

Three things can be wrong with an invitation and each says so rather than
appearing to do nothing. Steam may never have said which game it is for — an
invitation carries a connect string and no app id, and which game it is is
whatever the person doing the inviting is playing. The account may not own that
game. And the game may not be installed, which is the one with something to
press: the panel offers **Install**, and says to accept again once it is ready.
Installing does not join — a download is minutes and no lobby is held open for
them.

An invitation arrives one of two ways and this session reads both, because
which one Steam uses is not something a client can settle for itself: the chat
service carries it as a message of its own kind, and `ClientInviteToGame` is the
older push that carries no timestamp at all. The same invitation by both routes
is one card — the connect string is its identity — and an unstamped one sorts to
the end of the column, because no stamp means *now* rather than 1970. An
invitation that came through the chat service comes back in the history as an
ordinary message whose body is the connect string; those are recognised and
drawn as the card they are, so a conversation opened an hour later does not show
a line of machinery.

Four things the panel says out loud rather than by drawing nothing, because
three of them look alike and only one of them is finished:

| | |
|---|---|
| the history is coming | *Reading the conversation…* |
| it came, and there was nothing in it | *Nothing has been said yet. Say something.* |
| it did not come | the reason, and **Try again** |
| a message did not go | the message stays, in the warning colour, with the reason under it — **A** sends it again, and nothing takes it away |

Steam refuses two ways and they ask opposite things of the writer, so they are
told apart: a message that is too long has to be shortened, and one refused for
going too fast has only to be sent again. Both were measured against the live
service rather than looked up, because Steam publishes neither and answers with
a bare number.

**While Steam is out of reach the conversation stays and the field stops.**
Reconnecting, or an account standing offline on Steam by its own choice: what
was said is still true and still on the screen, and the line under the field
says why nothing can be sent. When the connection comes back, the conversation
on screen asks for its history again — nothing said while this session was away
was pushed at it, and messages already drawn keep their places, because Steam's
own timestamp and ordinal are what a message is filed under.

**A friend removed while their conversation is open keeps the conversation and
loses the field.** What was said was said, and taking it off the screen would be
losing it silently; what stops is writing, and the line under the field says so.
The check is made against the roster on every keystroke and again on the far
side, in the CM session itself, immediately before a message goes out — a
roster is a second or two behind Steam at the best of times, and somebody
unfriended while a message was being typed must not have it sent.

Signing out, or a different account signing in, takes the whole of it: every
conversation, every unread count, everything typed and not sent, and every
answer still in flight from the connection before.

## Settings > Games > Steam

Three switches, and the first is the flag above as a setting somebody can press.

**Integration.** On, which is everything this section has described. Off, none
of it exists and Steam is an application like any other: no row at the head of
Games, no library column, no artwork, no download card, and Valve's own
`.desktop` entry back where the scan files it — in Internet, on an ordinary
machine — wearing the icon its package ships. Pressing it starts Steam and
Steam's own window appears, because nothing is hiding it any more.

It takes effect on the press rather than at the next start. The bar is read off
the disk again, the cursor stays on the row that was pressed, and the worker is
started or stopped with it. Turning it off asks Valve's client to shut down —
but only a client this session started, never one that was already running or
belongs to another session, and never one in the middle of a download. The one
thing that does not change is the controller: Steam started from its own entry
is still handed the pad this shell drives, because the client is that pad's
other driver and every Steam game's only road to it.

**Start with the shell.** Off. On, Valve's client is started in the background
as the session comes up — the same wake a game press asks for, less the loading
screen — so the first game of the day starts as quickly as the second. Nothing
is said about it while it happens and nothing is said if it fails: it is the
shell getting ahead of a press nobody has made. What it costs is the client's
memory for the whole session on a machine where nobody plays anything.

**Leave Steam running.** On, which is what this shell has always done: a client
started for one game is still up for the next. Off, it is asked to shut down
once the last Steam game's window has gone — five seconds after, which is a
margin around the one thing that looks like a game ending and is not, a window
carried from one screen to the other. Four things call it off: another game
starting, a launch the client has stopped to ask about, anything moving on or
off the disk, and a window of Steam's own that somebody asked to see. Whichever
it is, the moment is let go of rather than kept — closing the client twenty
minutes later because a download finally finished would be acting on a game
nobody remembers.

**Storage.** Steam's own Storage page, on the bar: the libraries Steam keeps
games in, the games in each, and **Add drive**. The row leading to it says how
many libraries there are. See "Storage" below.

**Install games to**, the first row of Storage. Ask every time. Only on a
machine with more than one Steam library, and then it lists them — each by the
name it was given in Steam or the drive it is on, with the free space on that
drive under it — after **Ask every time**. Asking puts the library panel up on
each install (see "Which library a game goes into" above); a library chosen
here is used without asking and becomes Steam's own default as well, so
Steam's window agrees. A chosen library whose drive is unplugged says "Not
connected" and stays ticked, and until it is back every install asks. Written
as `steam-install-to`, the library's path as Steam lists it; missing is asking.

The three are written to the settings file as `steam-integration`,
`steam-at-startup` and `steam-after-a-game`, on every machine including one
with no Steam installed: what they answer is what *this shell* does. A file
that says nothing about them — every file written before this page existed —
gets the integration on, the client started for a game, and left running after
one. `--no-steam` outranks all three and does not rewrite them: a machine
booted once with the flag comes back the next morning set as it was, and the
page says so instead of offering a switch that would change nothing.

## Storage: libraries, and moving games between them

Settings > Games > Steam > **Storage** is the shell's copy of Valve's own
Storage page (Steam > Settings > Storage), for somebody with a controller in
their hand:

```
Settings > Games > Steam > Storage          (2 libraries)

    Install games to      Ask every time          only with two or more
    Home                  3 games · 27 GB free of 250 GB   ████████▌──
    GamesSSD              1 game · 412 GB free of 1.0 TB
    Add drive             Keep games on another drive
```

**A library** is named as Install games to names it, and its row says how many
games it holds and how much room is left on its drive, in words and as the
bar a partition draws on Settings > Storage. Stepping in lists every game its
manifests list, largest first — Steam's own order on that page, and Proton and
the runtimes included, since they take room like anything else — then
**Repair folder** and **Remove drive**. A drive that is not plugged in says
"Not connected", because its games cannot be read.

**Pressing a game raises a menu** — Move, Uninstall, Cancel — and never starts
it: a press on a settings page out of habit must not start forty gigabytes of
somebody's evening, nor take one off. Both are greyed out while Steam is working
on the game (the row says "Steam is working on it"). **Uninstall** is the
game's own Uninstall, with its question first. **Move** puts up a panel in the
shape of the one that asks where a game is installed: the game, "Choose where to
move it. It needs 34 GB.", a button per other library with the room left there
("GamesSSD · 412 GB free"), greyed out and saying "not enough space" where it
will not fit, and **Not now**. Room is wanted even for a library on the same
drive: Valve's client copies the files across and counts the bytes as it goes.

**While a game moves**, a panel says where it is going with a bar and the per
cent on it, and offers **Hide** and **Stop**. Hidden, the move goes on and the
game's row on the page carries the same bar ("Moving to GamesSSD · 34%"); the
end is announced in the corner. Stopped, the game stays where it was. One move
at a time — Valve's dialog moves one game after another, and a second watch
would read the first one's progress — so a Move pressed on another game says
to wait.

**Repair folder** is Steam's Repair Folder and says when it has finished.
**Remove drive** is Steam's Remove Library: Steam stops using the folder and
nothing on the drive is deleted. It is asked about first, and it is not offered
at all for the library Steam is installed in, which Steam can never be without;
a library that still holds games, or that Install games to names, says why it
cannot be removed instead ("Move or uninstall its games first", "New games are
installed here").

**Add drive** lists the drives Settings > Storage lists, by the names it gives
them, that could take a library and have none: mounted, writable by the person
signed in, not what the machine starts from, and not already holding one of
Steam's libraries — which is Valve's own dropdown ("/run/media/…/GamesHDD —
423 GB of 915.8 GB free"). A drive is asked for a `SteamLibrary` folder at its
top, which is what Valve's list proposes for every drive it offers (`%s%c%s`
beside `SteamLibrary` in `steamui.so`), because Steam refuses a drive's top
folder outright. **Choose a folder** is its "Let me choose another location":
the shell's own folder picker, answered with "Keep Steam games in this folder".
When the library has been made, the cursor is put on its row.

### How it is done

Every change is made the way Valve's page makes it, through the client's own
interface — nothing here writes `libraryfolders.vdf`, which the client writes
out of its memory and would write over. The calls, read out of
`~/.local/share/Steam/steamui/chunk~*.js` and `library.js`:

| Row | Call |
|---|---|
| Add drive | `InstallFolder.AddInstallFolder(path)` |
| Remove drive | `InstallFolder.RemoveInstallFolder(nFolderIndex)` |
| Repair folder | `InstallFolder.RepairInstallFolder(nFolderIndex)`, finished by `RegisterForRepairFolderFinished` |
| Move | `InstallFolder.MoveInstallFolderForApp(appid, nFolderIndex)`, followed by `RegisterForMoveContentProgress` |
| Stop | `InstallFolder.CancelMove()` |

A library is found by its path in `GetInstallFolders`, as Install games to finds
one. A move's progress comes as `{appid, eError, flProgress}`: `eError` 20
(`Busy`) while it goes, with `flProgress` in per cent, and 0 when that game is
done; anything else is a refusal — 15 "folder already exists", 17 "has shared
content", 22 "can't be moved", the three Valve's dialog has a sentence for,
plus 12 (no room) and 16 (running). Adding refuses with a word Valve's page
looks up in its catalogue — `NoDriveRoot`, `NotEmptyFolder`,
`NotWritableFolder`, `NotExecutableFolder`, `DriveAlreadyHasLibrary`,
`FailedToAdd` — and removing refuses with the app that is using the library.
`lxb_steam::webui::Declined` sorts all of them, and the shell says each in its
own words and in every language ("That folder has other files in it. Choose an
empty one."); Steam's word goes to the log. A client that cannot be reached is
"Steam could not be reached. Try again in a moment."

What is shown is read off the disk — `libraryfolders.vdf` and one manifest per
game, when Settings is arrived at and every three seconds while the page is
open, with the drives — so walking to it never wakes Steam. A press does: it
needs the account the shell holds, as an uninstall does, and wakes the client
where it is not up.

**What was checked, and what was not** (2026-09-24). Photographed in the
nested shell against a scratch home holding a Steam with two libraries: the
page, a library's games and rows, the menu, the Move panel, Remove drive's
question, Add drive's list of this machine's own drives and the folder picker.
Every press there ended in "Steam could not be reached", which is the
unreached path working — the scratch session holds no account. The calls into
Valve's client are Valve's own, read out of its interface; **none of them has
yet been made against a live client**, because each changes a real library.
The answers are parsed by tested functions (`shelved`, `move_began`,
`move_heard`), but the first real Move, Add drive, Remove drive and Repair
folder are still to be watched.

