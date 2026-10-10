# Windows Apps

[Documentation](index.md) · [Project home](../README.md)

- [A package, built on Proton](#a-package-built-on-proton)
- [Setting it up](#setting-it-up)
- [The Windows Apps row](#the-windows-apps-row)
- [Two ways in](#two-ways-in)
- [A disc in the drive](#a-disc-in-the-drive)
- [A setup opens on a Windows desktop](#a-setup-opens-on-a-windows-desktop)
- [After an installer](#after-an-installer)
- [A place of its own for each game](#a-place-of-its-own-for-each-game)
- [Playing](#playing)
- [When it closes, its Windows closes](#when-it-closes-its-windows-closes)
- [A game's menu](#a-games-menu)
- [Compatibility](#compatibility)
- [Uninstalling](#uninstalling)
- [Where the games are kept](#where-the-games-are-kept)
- [Keeping it up to date](#keeping-it-up-to-date)
- [What is Proton's and what is the shell's](#what-is-protons-and-what-is-the-shells)
- [When something does not work](#when-something-does-not-work)

## A package, built on Proton

A Windows game that did not come from Steam had nowhere to go, so it has a
package of its own. Everything on this page exists on a machine that has
installed **`lxb-proton`**, and on no other. The shell looks for that program
beside its own executable, then on `PATH`, and looks again every five seconds,
so installing or removing the package while the session runs puts the row on
the bar or takes it off. It is optional on the terms
[`lxb-retroarch`](retroarch.md), [`lxb-heroic`](epic.md) and
[`lxb-rpcs3`](ps3.md) are, and version-locked to the shell, because the two
talk over one JSON record per line (`PROTOCOL` in
`crates/lxb-proton/src/report.rs`).

Games are run under [Proton](https://github.com/ValveSoftware/Proton), Valve's
Wine, inside Valve's own container runtime — the same two pieces Steam uses,
joined the same way. On a machine with Steam, the Protons the client already
keeps up to date are what your games run under and nothing is downloaded. On a
machine without, the package fetches one of its own.

The mark on the row, the column and every game whose own icon could not be
read is a window of four square panes, drawn on the shell's glyph grid
(`crates/lxb-proton/glyphs/windows.svg`) and installed into `share/lxb/glyphs`
with the package. The helper carries the same drawing inside itself
(`lxb-proton mark`), so a helper run from where it was built still draws it.

## Setting it up

On a machine with Steam there is nothing to set up. On one without, the first
press offers one free download: a Proton and the container runtime it asks
for, into your own data directory. Nothing else on the machine is changed,
nothing is installed as root, and no account is signed in to.

Each file is checked against the digest published beside it before it is
unpacked, the runtime is checked again with Valve's own `pv-verify`, and both
are unpacked into a staging directory and moved into place only once they are
whole. A download that is interrupted leaves nothing behind, and asking again
starts the unfinished half over.

## The Windows Apps row

**Windows Apps** stands in the Games column directly under Steam, and opens a
column of its own. The line under it says what pressing it would do: that it
is looking, that it needs setting up, or how many games there are.

Its own menu has **Sort** and, where there is nothing to run games with yet,
**Install**.

## Two ways in

Two rows at the head of the column, before the first game:

1. **Run a Windows program** walks your disk for something Windows would run —
   an installer, or a game that needs none. Folders and the four endings
   Windows runs (`.exe`, `.msi`, `.bat`, `.cmd`) are all the walk shows, so
   there is nothing on it that cannot be the answer.
2. **Add a game or app folder** copies in something you already have, whole.
   For a game or a program that arrived without an installer: a folder from
   another machine, one an archive was unpacked into, or one on a disk you
   would rather not keep plugged in. A folder rather than a file, because an
   executable without the files beside it is a program that starts and stops
   — and because the folder you point at is the only way the shell is ever
   told which folder is a program's.
Running something *inside* a game that is already set up — a mod's installer,
a patcher, a tool that wants the game there before it will start — is on that
game's own menu rather than up here. It walks your disk the same way this row
does; what differs is where what you pick ends up.

A game is named after itself rather than after the file you downloaded. An
installer called `npp.8.9.8.Installer.x64.exe` becomes **Notepad++**, because
that is the name the program carries inside it — the one Windows shows on a
file's Details tab. The filename is used only when a program says nothing.

It wears its own picture for the same reason: the icon the program's author
put inside the `.exe`, read out and put on the row, which is the drawing you
already know the program by. A program carrying none keeps the window mark.

## A third way in: the file itself

A Windows program met in **System > Files** is a Windows program, and the
column is not the only place it can be run from. With this package installed,
any `.exe`, `.msi`, `.bat` or `.cmd` in a folder:

* **wears the window mark** instead of the page every other unknown file
  wears, so the one file in a folder that will do something when it is pressed
  looks like it;
* **wears its own icon**, where it carries one — the same drawing the Windows
  Apps column puts on a game's row, read out of the same file, so a folder of
  five installers looks like five programs instead of five identical pages.
  An `.exe` and an `.msi` are asked about; a `.bat` and a `.cmd` are text
  files with nothing inside them to read, and keep the window mark;
* **runs when you press it.** A file inside a game's folder runs in *that*
  game's Windows; anything else is a program this machine has not got yet, so
  it is given a Windows of its own and started — the same rule, and the same
  loading screen, as **Run a Windows program**. A press on a machine that has
  the package but has never downloaded a Proton offers that download first,
  exactly as the Windows Apps row does. **It does not have to be an
  installer**: a program that needs none works the same way, and the next
  section is how.
* And its **Options** menu carries **Run inside a game**, which lists the
  Windows games already set up and runs the file inside the one you choose.
  This is **Run something inside** from the other end: that one starts at the
  game and walks your disk for the file, this one starts at the file and lists
  the games. Whichever half you happen to be standing on, the act is the same
  one. The row is there only when there is a game to run it inside of.

None of it appears without the package, and none of it appears while
**Settings > Windows Apps** is switched off: the file goes back to being a
page, and a press on it does whatever the desktop would have done anyway.

The icons are drawn wherever a file row is drawn — a folder in Files, the walk
**Run a Windows program** opens, and the file panel an application raises to
ask you for a file. One file, one mark, whichever column you are looking at it
from.

## An installer and a program that needs none

Press either and the same thing happens, because **nothing on a disk reliably
says which of the two a file is**. A great many installers are named for a
download, ask for no special permission, and carry the product's own name and
icon inside them — exactly as the program itself does. So neither you nor the
shell is asked to guess:

1. **What you pressed runs where it already is**, in a Windows of its own,
   with the folder it lives in shared into that Windows. Nothing is copied
   anywhere, so a program that needs no installer has everything it came with
   — its plugins, its languages, its data files — exactly as it had on your
   disk a moment before.
2. **It is on the bar and ready to press from that moment**, under its own
   name and wearing its own icon.
3. **When it closes, the shell looks in the Windows it ran in.** Anything in
   there is something that run put there, since nothing else ever went in: so
   it was an installer, and what it installed is what the row plays from then
   on. An empty one means what ran *was* the program, and the row goes on
   playing it where it lives.

**Nothing of yours is copied, moved or changed.** The row is a way back to the
program you already have, not a second copy of it — so pressing a `.exe` costs
no disk, and taking the row off again leaves your own files exactly where they
were.

What that asks in return is that the program stays where it is. Move the
folder or delete the download and the row says the program is not where it
was, which is the truth and is fixed by pressing the file again in its new
place.

**To have a copy of your own**, so that the download can go: **Add a game or
app folder** at the head of the column copies the whole folder you point at
into a Windows of its own. That is also the way in for a program on a memory
stick you would rather not keep plugged in.

**Pressing the same program again** runs the one you already have rather than
standing a second row beside the first. An installer pressed again is a
different matter — that is somebody installing it again, and it gets a new
Windows of its own, which is what it has always done.

## A disc in the drive

A CD or DVD made for Windows is a way in of its own. Put one in the drive and
it stands at the head of the games in the Windows Apps column, under the two
rows above, called what the disc calls itself and wearing the disc's own
picture. If you are on the start screen and doing nothing else when it goes
in, the cursor is taken to it, so the next press is the one that installs it.
A disc that was already in when the session started is on the column too, and
nothing moves for it.

**Which discs.** The ones Windows itself would open: a disc whose
`autorun.inf` names a program runs that program, wears the picture it names
and is called by the name it gives, as it would be on Windows. A disc without
one is offered the plain way in at its top — a program called `setup`,
`install` or `autorun`, or the only program there is. A disc whose programs are
all further down than its top gets no row: that is a disc of somebody's files,
and choosing one of its programs would be the shell choosing for you. Films,
music and photographs get none either.

**Pressing it** runs the disc's program from the disc, in a Windows of its
own, in the middle of the screen — the same installer as a program pressed in
Files, described [just above](#an-installer-and-a-program-that-needs-none).
The first press builds that Windows
before the program starts in it, so the disc is a CD drive with a drive letter
of its own there, just as it is on Windows: a setup that asks for its second
disc looks for it on that drive, and a game that reads its disc while it plays
finds it where it expects. That press takes a little longer, and the loading
screen stays up while it happens.

**What it installs becomes a game** in the column, with its own name and
picture. From then on the disc's row reads *Ready to play* and plays that
game, which suits an older game that checks for its disc: the press that put
the disc in is the one that starts it. **Install again** on the disc's menu
runs the disc's program once more, into a Windows of its own.

**If nothing was installed** — you looked at the disc's menu and closed it, or
the game plays straight from the disc — the Windows it ran in is kept, but it
gets no row of its own. The next time that disc goes in, its row runs in that
same Windows, so a game played from the disc keeps its saves. **Uninstall** on
the disc's menu takes that Windows off, saves included.

**Eject** on the disc's menu opens the tray. A disc cannot be taken out while
a program on it is still running.

A disc is recognised by the name and the date written on it, not by where it
was put, so the same disc is the same disc every time it goes in — and two
discs that happen to share a name are not.

## A setup opens on a Windows desktop

An installer gets a Windows desktop of its own, the size of the display it was
started on, and opens in the middle of it. So does anything else you run
inside a game that is already set up — a mod's installer, a patcher, a
configuration tool — each at its own size, and each where it would open on
Windows, which for an installer is the middle.

The reason is that an installer is one small window of a fixed shape. Every
application on this bar is given the whole display, which is what a game
wants; an installer handed the same thing goes on drawing its own small
rectangle in the corner of it and leaves the rest blank. A desktop is a window
that *can* take the whole display, and the installer sits in the middle of it,
exactly where it puts itself on Windows.

**A game does not get one.** The game is what the screen is for, and one that
changes resolution needs the screen itself to change with it.

## After an installer

When a setup finishes, the shell looks in the game's folder and takes what it
left. You do not have to find it: the row goes from the setup you pressed to
the game it installed, on its own, as soon as the installer closes.

What it takes is the likeliest program in there, which is not the first file
found — a setup leaves uninstallers, crash handlers and redistributables
behind, and those rank last. If it picks wrong, **Choose what to play** on the
game's menu lists everything in the folder and fixes it in one press.

A game can be a `.bat` or a `.cmd` as well as an `.exe`: a pack built round an
emulator or a Java program often ships `start.bat` and nothing else. A launcher
script is a few hundred bytes against a game's megabytes, so where there is a
program beside one the program wins. An `.msi` is never offered as the thing a
game plays — it is what puts the game there, not the game.

A setup you cancelled installed nothing, so the row goes on playing the setup
where it lives — which is how an interrupted install is finished: press the
row and the same installer runs again, in the half-built Windows rather than
in a new one. When it gets to the end, the game names itself. **Run something
inside** on the game's menu does the same from any other installer on your
disk.

## A place of its own for each game

Every game gets its own Windows installation rather than sharing one. A game
that needs an old library cannot break the game beside it, a setup that goes
wrong is one game's problem, and taking a game off takes nothing else with it.

What that costs is disk: each one is a Windows installation of a few hundred
megabytes before the game itself. What it buys is that no two games can
interfere, which is the failure that is hardest to undo.

## Playing

A game starts full screen behind the loading screen, on the display it was
pressed on, and the guide's **Close** ends it. The first start of a game takes
longer than the rest: the Windows installation is built then, and after the
machine has been restarted that can be over a minute. The loading screen stays
up for as long as it takes.

A game that starts a small launcher and hands over to the real thing is
followed properly. The shell waits for everything the game started rather than
for the first program it ran, so a game is not filed as closed while you are
playing it — and Close takes the whole game down rather than leaving pieces
running where nothing can reach them.

## When it closes, its Windows closes

Close the application yourself, or press **Close** on the guide, and the whole
Windows installation it was running in goes with it.

That is not automatic, and it is why this is written down. Windows under Linux
is not one program: a server process holds the installation open and keeps
holding it for as long as anything at all is still using it, so an application
that leaves one piece of itself behind — a launcher that did not notice, a
background service that hung on the way out — can leave a whole Windows
running with nothing on screen to say it is there. It costs memory and it
keeps a disk busy, and the only sign of it is a machine that feels slower than
it was.

So when the last thing you started in a game finishes, however it finished,
the shell asks for everything still running in that game to stop: politely
first, so anything with saves to write gets to write them, and then not
politely. A game you are still playing is never touched by this — running a
mod's installer inside a game that is open ends the installer and leaves the
game alone.

## A game's menu

- **Play.**
- **Choose what to play**, for a setup that was interrupted, or that guessed
  the wrong program. It lists what is inside the game's own folder, likeliest
  first, with the one in force marked. It is also what a press on a game that
  cannot be played yet opens.
- **Run something inside**, for a mod's installer, a patcher, or anything else
  that wants the game already installed. It walks your disk for something
  Windows would run — your Downloads, a USB stick, anywhere — and runs what
  you choose *in this game's Windows*, so a mod lands in the game rather than
  in a Windows of its own. Nothing new is created and what the game plays does
  not change.

  This is the difference between it and **Run a Windows program** at the head
  of the column: that one gives what you pick a Windows of its own, this one
  puts it in the Windows a game already has. It is the same walk of the same
  disk either way, so while it is open for a game the row it came out of says
  which — **Run inside Cemu**, not **Run a Windows program**.

  `.exe`, `.msi`, `.bat` and `.cmd` can all be chosen, because a mod is
  shipped in whichever of them its author reached for. An `.msi` is handed to
  Windows Installer, which is the program that reads one; the rest are run as
  themselves.
- **Compatibility**, while Steam is installed. See below.
- **Uninstall.**
- **Resolution**, set per game.
- **Sort**: games you can play first, then by name either way. Games you can
  play come first by default, because a setup that was never finished cannot
  be started.

## Compatibility

Every Proton on the machine, with the one in force marked, and **This
machine's choice** at the top to give a game its default back. A Proton whose
runtime is missing is shown greyed rather than offered.

It appears only when Steam is installed. Without Steam this machine has the
one Proton this package fetched, and a list of one is a question with no
answer in it.

The choice is kept on the game, not applied once. It is kept even if Steam is
later taken off the machine: the game then runs under the Proton this package
fetched, and when Steam comes back the choice is in force again without
anybody having to make it twice.

## Uninstalling

**Uninstall** asks first, and says what goes: everything in that game's own
folder, saved games included. They live inside the game's own Windows
installation, so they go with it. It cannot be undone.

It is refused while the game is still running. What counts as running is a
live process using that game's Windows installation — not a lock file, which
Proton releases before the game starts.

Only that one game's folder is deleted, it is checked to be one of ours
first, and no link out of it is ever followed.

## Settings > Windows Apps

A page of the Settings column itself, beside Games rather than inside it. It
began under Games and that was wrong about what it is: the other pages there
are each a way of playing games and nothing else, and this one runs Windows
*programs* — a tool, a patcher, a launcher, an installer — of which games are
the common case and not the whole. Filed under Games it said that playing is
the only reason this machine runs a Windows program.

**Integration**, on by default — a machine that installed the package asked for
it. Off, there is no Windows Apps row and no column, and nothing on the bar
plays a Windows game.

It is the one of these switches whose Off puts no application back on the bar,
because none came off for it: Proton is what a Windows game is run *with*, not
a program with a window and an icon of its own. So what Off says is what is
true — your games stay on the disk, and nothing on the bar reaches them.
Switching it back on finds every one of them again, where it was, with its own
picture and its own name.

**It is the Windows Apps integration and nothing else.** Steam goes on running
its own games under its own Proton, and Heroic goes on running Epic games under
its own; those are two other programs' settings, kept in those programs' own
files, and this switch does not reach either of them.

It takes effect on the press. A Proton coming down or a game folder being
copied in stops with it — a game whose copy was cut short comes back saying its
setup never finished, which is what the column already says about one, and
pressing it offers to finish it. A Windows game already running is left running
and plays to its end.

Written as `windows-integration`, on every machine including one that has never
had the package. A file that says nothing about it gets the integration on.

**Games and Apps folder** is the other row, and the whole of the rest of the
page. It is where new ones are kept, and it opens the ordinary folder walk to
change it. It is called that and not "Games folder" for the reason the page is
where it is: what lands in there is as likely to be a program as a game. It is
the same row that stands at the head of the Windows Apps column while nobody
has chosen a folder — one setting, one row, in both places.

Everything else about a Windows program belongs to that program and is on its
own menu, where the thing it is about is on the screen beside it. A page of
settings that each needed a program chosen first would be a page you could not
use from here.

## Where the games are kept

`~/.lxb-proton`, unless you choose somewhere else. Games already installed
stay where they are and are still found, so moving the folder does not strand
them.

One row is not kept there: a program pressed in **Files** is played where you
keep it, and only its Windows is in the games folder. That Windows is a few
hundred megabytes whatever the program is, and takes the program's saves and
settings with it when the row is removed — the program itself is yours and is
left alone.

## Keeping it up to date

The Proton this package fetched is the one thing on the machine that no
package manager will ever update, so **Settings > Updates > LineXinBar** does
it. The row appears there whenever a Proton of ours is installed — including
on a machine whose LineXinBar came from its distribution's repositories, where
that row would otherwise not be there at all.

It needs no password: it writes into one directory under your own data
directory and nowhere else. Steam's Protons and Steam's runtime are never
touched; the client keeps those up to date itself. An update is refused while
a Windows game is running, and it applies the next time a game is started.

## What is Proton's and what is the shell's

Proton builds and upgrades the Windows installation, and runs the game.
Valve's container runtime provides the libraries it runs against. Everything
about where a game is, what it is called, what it runs under and when it is
finished is the shell's.

The shell starts the game itself rather than letting the helper do it, so the
game gets the loading screen, the display it was pressed on and the guide's
Close, exactly as a Steam game or a ROM does. The helper works out the command
and hands it over.

**No Steam application id is invented for these games.** Proton reads one to
decide which per-title workarounds to apply, so a made-up number would quietly
opt a game into another game's fixes; and the shell reads the same number back
the other way to decide a window belongs to a Steam game, so a made-up one
would file your Windows game as Steam's. The cost is that every Windows game's
windows carry the one name Wine gives them without an id, which is why a
game's chosen resolution is sent on the way into that game's own launch.

## When something does not work

- **The row is not there.** The package is not installed, or the shell has not
  looked yet — it looks again every five seconds.
- **The row says it needs setting up.** There is no Proton on the machine yet.
  Press it.
- **A game says its setup was never finished.** Press it: the shell looks in
  the folder again and offers whatever the setup left. Nothing offered means
  the installer was closed before it wrote anything — use **Run something
  inside** on its menu to run that installer again in the same place.
- **A game plays the wrong program.** A setup that leaves several executables
  is a guess, and **Choose what to play** on its menu is where the guess is
  corrected. The choice is kept.
- **A download will not finish.** Each one is checked, and a file that did not
  arrive whole is never kept. Try again.
- **An update is refused.** A Windows game is still running. Close it.
- **A game will not start after Steam was removed.** It falls back to the
  Proton this package fetched; if there is none, **Settings > Updates** or the
  row's own **Install** will get one.
