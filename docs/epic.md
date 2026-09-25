# Epic Games

[Documentation](index.md) · [Project home](../README.md)

- [A package, built on Heroic](#a-package-built-on-heroic)
- [The Epic Games row](#the-epic-games-row)
- [Signing in](#signing-in)
- [The Epic Games column](#the-epic-games-column)
- [A game's menu](#a-games-menu)
- [Installing, updating and uninstalling](#installing-updating-and-uninstalling)
- [Playing](#playing)
- [Playing without a connection](#playing-without-a-connection)
- [While Heroic is open](#while-heroic-is-open)
- [Games from the EA app and Ubisoft Connect](#games-from-the-ea-app-and-ubisoft-connect)
- [Settings > Games > Epic Games](#settings--games--epic-games)
- [Achievements](#achievements)
- [What is Heroic's and what is the shell's](#what-is-heroics-and-what-is-the-shells)
- [When something does not work](#when-something-does-not-work)

## A package, built on Heroic

Everything on this page exists on a machine that has installed **`lxb-heroic`**
and on no other. The shell looks for that program on `PATH` at startup (beside
its own executable first, so a local build never picks up an older installed
helper) and, without it, never mentions Epic Games at all: no row, no column, no
page under Settings, and Heroic Games Launcher is on the bar as an application
like any other, with its own icon. It looks again every five seconds, so a
package installed or removed while the session runs is followed at once: taken
off, the Epic Games row, column, achievements and page go and Heroic's own entry
comes back; put on, the integration starts as a session coming up starts it. (A
Nix install carries the helper in the one package, so there it is always on;
the Integration switch under Settings is the way to turn it off.) It is optional
on exactly the terms `lxb-retroarch` is — see
[RetroArch](retroarch.md) — and version-locked to the shell, because what the two
agree about is one JSON record per line on a pipe (`PROTOCOL` in
`crates/lxb-heroic/src/report.rs`), not a library.

The integration is built on **Heroic Games Launcher's Flathub build**
(`com.heroicgameslauncher.hgl`), not on a copy of legendary of its own. The
helper runs the legendary that Heroic bundles, inside Heroic's sandbox, pointed
at Heroic's own configuration, so the account, the library, the installed games
and the settings the bar shows are Heroic's — open Heroic's window and it shows
the same things. A user or a system-wide installation of the flatpak is used as
it is; the helper installs one only where there is none, and then into the
user's own flatpaks, which needs no password.

The mark on the row, the column and every game without a cover is Epic's own
shield (`crates/lxb-heroic/glyphs/epic.svg`), installed into
`share/lxb/glyphs` with the package.

## The Epic Games row

An **Epic Games** row stands in the Games column under Steam and above
RetroArch. Heroic's own `.desktop` entry comes off the bar where this row stands
in for it; Heroic's window is one press away, as **Open Heroic** in the row's
menu. The line under the row says what a press would do, and the press does the
one thing that is left:

1. **Heroic is not installed.** "Press to download it". The press asks, and Yes
   installs Heroic from Flathub into your own flatpaks with a bar counting it
   up. A machine with no flatpak at all says it cannot be downloaded here.
2. **Heroic has nothing to run games with.** "Press to finish setting it up".
   The press downloads **Heroic's own default Proton** (Proton-CachyOS, from the
   same place Heroic's Wine Manager fetches it, checked against its published
   checksum) into Heroic's `tools/proton` and makes it Heroic's default. A
   Proton Heroic already has is adopted rather than downloaded again. Without
   one, Heroic's first launch of a game stops to ask a question in a window
   the shell keeps off the screen.
3. **Nobody is signed in.** "Sign in to play your Epic Games library here".
   The press opens the sign-in panel.
4. **Signed in.** "Signed in as …". The press steps into the Epic Games column.

The row's menu is the Steam row's: **Refresh the library** (ask Epic for it
again) and **Sign out** (asked first; installed games stay on the disk), then
under the rule **Sort**, **Open Heroic** — where the Steam row has Open Steam
— and Cancel. Heroic's window is only here, never on a game's menu.

## Signing in

Epic's sign-in is a web page behind bot protection, so there is no password
field for it anywhere in the shell. There are two ways in, both Epic's own page:

- **With a phone** (the panel's first answer). A QR code, and under it the
  address and a short code for anybody typing it on another device. Approving
  it on the phone — two-factor authentication included — signs Heroic in. The
  code lasts ten minutes and is replaced when it runs out. Epic's page may say
  **Fortnite** is asking: the phone way borrows that client's device sign-in,
  and the panel says so, because it is otherwise alarming.
- **On this screen.** For somebody with no phone or second device to hand,
  **Sign in on this screen** opens the same page, with the same code already
  filled in, in the browser this machine opens web addresses with. Signing in
  there approves the code, so the sign-in finishes exactly as it does for a
  phone, and the bar comes back over the browser when it has.

Back gives a sign-in up. Nothing is kept until a code has been approved; the
account Heroic then holds is the one that approved it. Refused while Heroic is
open, for the reason in [While Heroic is open](#while-heroic-is-open).

## The Epic Games column

Signing in adds an **Epic Games** column after Steam's: the account's games, by
Heroic's own rules for what a game is (no Unreal Engine content, no mods, no DLC
and no phone-only releases). It is Steam's column in every way that can be
pressed:

- **Search** stands over it — the field that narrows it as a name is typed, on
  the frame the letter is, and the row that empties it.
- **Alphabetical** stands under that: every game under its first letter, each
  letter installed-first, built by the same index Steam's column uses.
- **Sort** lists it in Steam's eight orders — installed first (the default),
  name either way, recently played, play time either way, size either way —
  over what Heroic knows: whether a game is here, its name, when Heroic last
  started it, Heroic's playtime and its size on the disk. The choice is kept as
  `epic-sort` in `shell.toml`. An order nothing can be sorted by yet is greyed.

- Each game is a card of **Epic's tall box art**, 3:4, filled edge to edge. The
  few boxes that are not 3:4 get a card of their own measured shape. A game not
  on this disk is drawn colourless.
- Standing on a game puts its wide art behind the whole display, as a Steam
  hero is.
- The line under a game says whether it is installed, how large, how long it has
  been played ("Installed · 378 MB · 5 min played"), whether an update is
  waiting, or which store plays it.
- Covers are fetched once, six at a time, into the shell's own cache
  (`~/.cache/lxb/epic-art`); a backdrop and a logo only when the cursor reaches
  the game.

## A game's menu

Steam's game menu, row for row and band for band:

- **Play**, or **Install** for a game that is not here, or **Stop installing**
  for one that is on its way — with **Update now** first where Epic has one
  waiting (Steam updates its games itself; Heroic's shortcut does not).
- **Verify and repair**: legendary's own `repair`, Heroic's Verify and
  Repair. Every file is checked against Epic's manifest and whatever is wrong
  is fetched again, in the download queue: the row counts "Checking files…
  45%" and then "Repairing…", the guide's corner card says Checking, and a
  quiet notice says every file has been checked.
- **Compatibility**: **Heroic's default** first, then everything Heroic can
  run the game with — found where Heroic finds it: its own Protons and Wines,
  every Steam library's `compatibilitytools.d`, Lutris's Wines. Choosing one
  writes the game's own `wineVersion` into Heroic's `GamesConfig`; Heroic's
  default takes it back out. Nothing is asked and nothing is put on the screen.
- **Uninstall**, asked first.
- **Resolution**: how many pixels the game draws, as for any application. Every
  game Heroic starts puts the same X11 class on its window (`steam_app_0`), so
  the answer is filed under the game (`epic:<app>` in `apps.toml`) and sent
  under that class on the way into that game's own launch — the arrangement
  the ROM folder uses for RetroArch's one window name.
- **Sort**, and Cancel, under the rule.

## Installing, updating and uninstalling

**Pressing a game that is not here** asks whether to install it, with what it
takes and whether it fits:

> Cat Quest · 137 MB to download, 378 MB once installed. · 369 GB free on this machine.

A game there is not room for says so and offers only Close. Install is Heroic's
own install, with Heroic's own download settings, into Heroic's own install
folder.

- **One download at a time.** More wait their turn ("Waiting to download"). The
  game's row counts up — "Installing… 45% of 131 MB" — with a bar, and so does
  the Epic Games row, so it can be watched from the Games column. The guide's
  corner has it too, on the card a Steam download stands on: "Downloading Cat
  Quest", the game's cover (Epic publishes no icon) and the same bar. A Steam
  download, or a Steam game somebody is waiting in front of, has the card
  first; one card, because it is one fact about the machine.
- **Pressing a game that is coming down** asks whether to stop. Stopping keeps
  what has arrived: installing it again carries on from there.
- **When it lands** it moves to the top of the column and the cursor goes with
  it, and a notice in the corner, without a sound, says the download has
  finished. A download that does not land says why in plain words — too little
  space, Epic not reachable, Heroic open.
- **An update** Epic has published shows as "Update waiting" on the row.
  Pressing the game asks whether to **Update now** or **Play** what is here —
  Heroic's shortcut starts a game without checking, so an update nobody is
  asked about would never happen — and the game's menu offers Update now first.
  An update counts up as "Updating…", measured against what actually has to come
  down.
- **Uninstall** is in the game's menu, and asked first with **Keep It** standing
  highlighted. Games from the EA app and Ubisoft Connect are taken off Heroic's
  list; everything else is removed from the disk.

## Playing

An installed game starts through **Heroic's own shortcut**,
`heroic://launch?appName=…&gui=false`, so Heroic's window never appears. The
loading screen is a **Steam game's**: no panel, the game's own picture across
the display, its logo in the middle where Epic publishes one (most Epic games
have none, and their name is written there instead), the ring and "Starting the
game" in the corner, and a dip through black onto the game's window. It waits
up to two minutes; a Heroic starting cold, preparing a Proton prefix, takes about
half a minute. umu's own brief "ProtonFixes" window is not mistaken for the game.

The guide's **Close Cat Quest** ends the game the way it ends any other. Heroic
records the playtime and exits by itself, and the row shows the new time a
moment later. With cloud saves on, Heroic syncs the saves on the way in and out.

A game that does not start is said — "It did not start." — rather than the
loading screen simply going. Where no Heroic was running at the press, the
process the shell started is Heroic itself, so its going with no window is
the answer at once; where one was, the press is a hand-over and the answer is
the two minutes running out. Why is in Heroic's own log for the game
(`~/.var/app/com.heroicgameslauncher.hgl/.local/state/Heroic/logs/games/`).

## Playing without a connection

**Heroic plays offline by itself.** When it cannot reach the internet it starts
the game with legendary's `--offline`: no sign-in, no save sync and no update
check, and the playtime is still kept. Measured on 2026-09-25 with every
request from the session's programs sent to a dead proxy: Cat Quest started in
seven seconds, legendary never tried to sign in, and the row read one minute
more afterwards.

The shell hears from **NetworkManager** whether the machine has a way out
(`Connectivity`, read on the pass that keeps the start screen's wireless mark
true). Only a plain "none" counts; a machine with no NetworkManager is never
taken for offline. With no connection:

- The Epic Games row says "Signed in as … · No connection".
- An installed game Epic does not mark as running offline (`CanRunOffline`)
  says "Installed · May need a connection to play". Most games are marked; of
  the rest, many run perfectly well, which is why it says *may*.
- A game with an update waiting plays what is here instead of asking about
  the update, which could not come down.
- The install question says Epic could not be reached and offers only Close.
- A game that did not start says, under "It did not start.", that it may need
  a connection.
- The Trophies column keeps its Epic achievements: the last answer Epic gave
  for each game is kept beside its icons and handed back when Epic cannot be
  asked.

**The first launch of any game needs a connection once.** umu, which Heroic
starts games with, fetches its Steam runtime the first time
(`~/.local/share/umu`, shared with any umu on the machine); after that it plays
offline.

## While Heroic is open

Heroic reads what is installed, its account and its settings when it starts and
keeps them. So while Heroic is open — its window, or a game started through it —
the helper changes none of them:

- A game asked for is **not refused but held**: it waits at the head of the
  queue ("Starts when Heroic and its games are closed") and starts by itself
  when Heroic has gone.
- Uninstalling, signing in or out, the install folder, what games run with and
  cloud saves say "Heroic is open, or a game from it is running. Close it, then
  try again."

**Except the shell's own background Heroic.** With Start with the shell or
Leave Heroic running on, a Heroic with no window of its own and no game running
(nothing with `HEROIC_APP_NAME` in its environment) is the shell's. The helper
is told so (`LXB_HEROIC_IDLE`) and goes ahead; afterwards the shell closes that
Heroic — `SIGTERM`, which Electron takes as quitting — and starts it again, so
what it read as it started is true again. A download held for Heroic goes on
the moment that is the only Heroic up.
- A game that finished installing while a Heroic the shell started was already
  running is not handed to that Heroic, which would not know it is installed;
  the press asks for it to be closed first.

## Games from the EA app and Ubisoft Connect

Some Epic purchases are played through another store. They are installed **as
Heroic installs them on Linux**: that store's installer is fetched into Heroic's
own folder for it and the game is marked installed. The **first time it is
played**, the installer runs inside the game's prefix, and the game itself then
comes down **in that store's own window**, which asks for its own sign-in. That
window is the one thing on this page the shell cannot take over. The row says
"Plays through Ubisoft Connect" or "Plays through the EA app", and the install
question says where the game will come down.

## Settings > Games > Epic Games

On a machine with the package, Settings > Games has an **Epic Games** page —
Steam's page, for Heroic:

- **Integration**, on by default. Off, the shell has nothing to do with
  Heroic: no Epic Games row, column or achievements, and Heroic's own entry is
  back on the bar as an application like any other. Switching it off stops a
  download coming down (installing it again carries on) and closes the
  background Heroic; switching it on starts the integration as a session does.
- **Start with the shell**, off by default. On, a Heroic is started in the
  background as the session comes up — `heroic://launch?gui=false`, Heroic's
  own shortcut naming no game, which is the one way it starts without its
  window — so the first game starts sooner. Turned on, it starts one now.
- **Leave Heroic running**, off by default where Steam's is on: Heroic is up in
  a second, and one left running is one the shell has to restart whenever it
  changes something Heroic knows. Off, games start with `--no-gui`, which makes
  Heroic close with its game. On, they start without it, detached (`setsid
  -f`), so Heroic stays; the process the shell watches is then a courier that
  hands the game over, the loading screen follows the game's window, and the
  game's end is Heroic writing its playtimes (`store/timestamp.json`), which is
  how the shell notices any game of Heroic's ending, wherever it was started.

Then Heroic's own settings, written where Heroic keeps them, never a copy:

- **Install games to** — where new games go: Heroic's `defaultInstallPath`,
  chosen with the ordinary folder picker. A game already installed stays where
  it is. A folder Heroic's sandbox cannot reach (it reaches `~/Games/Heroic`,
  `/mnt`, `/media` and `/run/media` by itself) is granted with a per-user
  flatpak override, which needs no password.
- **Run games with** — Heroic's default `wineVersion`, chosen from the same
  list a game's Compatibility offers, and written into both `config.json` and
  the `store/config.json` Heroic's window shows.
- **Cloud saves** — Heroic's own `autoSyncSaves`. Turning it on also finds the
  save folder of every installed game that keeps saves with Epic and writes it
  into that game's Heroic settings, the way Heroic's own per-game switch does;
  a game installed later gets its folder as its install finishes. Heroic then
  downloads a game's saves before it starts and uploads them after it ends —
  seen on 2026-09-25 with Cat Quest: "Saves for Cat Quest downloaded", and
  after the game "Saves uploaded for Cat Quest".

## Achievements

Epic games stand in the **Trophies** column beside Steam's and
RetroAchievements', as a card of the game's cover — "Epic Games · 3 / 12
unlocked" — with its achievements under it in Unlocked and Locked sections, each
with Epic's own icon, its XP, when it was unlocked, and how many players have
it. A hidden achievement stays hidden until it is unlocked.

As on Steam's side, **every game the account owns that has achievements** is
there, played or not, on this machine or not — unlocks earned on another
computer included. legendary keeps each game's achievement list beside its
store record, so which games those are is read off the disk (60 of 221 on the
machine this was written on); what the account has unlocked is asked of Epic
with legendary's own `achievements` command, four games at a time, once a
session — about twenty-five seconds for sixty games, in the background.

- **The last answers first.** Each game's last answer is kept beside its
  icons, and all of them are handed over before Epic is asked again, so the
  column is whole the moment the session starts and each game is brought up to
  date as Epic answers. With no connection the kept answer is the answer.
- **Icons when a game is opened.** A whole library's icons are thousands of
  pictures, most never looked at; like Steam's, a game's are fetched when its
  list is opened, and kept.
- **After playing**, only the games started from the column are asked about
  again.

## What is Heroic's and what is the shell's

| Heroic's (under `~/.var/app/com.heroicgameslauncher.hgl/config/heroic`) | The shell's |
| --- | --- |
| The Epic session (`legendaryConfig/legendary/user.json`) | Covers, backdrops, logos, achievement icons and the last achievements Epic gave (`~/.cache/lxb/epic-art`) |
| The library and what is installed | Nothing else: no setting of the integration's own |
| Settings: install folder, Proton, cloud saves, per-game save folders | |
| Playtime (`store/timestamp.json`) | |

Games install to Heroic's folder (`~/Games/Heroic` by default), and every game
shares Heroic's default prefix (`~/Games/Heroic/Prefixes/shared`).

## When something does not work

What a person reads says what to do; why is in the session log. Every helper
line the shell cannot use, every legendary line that is not progress, and every
refusal's reason is logged by the shell and by `lxb-heroic` on its stderr.
Running the helper by hand shows the same thing the shell sees:

```text
lxb-heroic probe             is Heroic here, whose account, which Proton
lxb-heroic library --refresh the account's games, asked of Epic first
lxb-heroic size APP          what installing one would take
lxb-heroic achievements      what has been unlocked
```

Two things worth knowing:

- A session started from some editors and terminals carries
  `ELECTRON_RUN_AS_NODE=1`, which turns Heroic into plain Node (`bad option:
  --no-gui`). The shell clears it for every Heroic it starts; clear it by hand
  when running `flatpak run com.heroicgameslauncher.hgl` yourself.
- A download stopped after its last file was written is finished the next time
  it is asked for: legendary on its own would say there is nothing to download
  and never install it, so the helper imports that copy, checks every file of
  it against Epic's manifest, and removes the stale resume data.
