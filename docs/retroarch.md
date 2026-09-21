# RetroArch

[Documentation](index.md) · [Project home](../README.md)

- [RetroArch, and your own console games](#retroarch-and-your-own-console-games)

## RetroArch, and your own console games

**A package, not a feature.** Everything in this section exists on a machine
that has installed **`lxb-retroarch`** and on no other: the shell looks for that
program on `PATH` at startup and, without it, never mentions RetroArch anywhere
— no row, no column, no page under Settings. It is a separate package because a
machine that will never emulate a console should not carry a table of forty
consoles, a flatpak installer and a walk over somebody's collection. See
`crates/lxb-retroarch`, which is the whole of the optional half; the shell's own
half is `src/retroarch.rs`, and what passes between them is one JSON record per
line on a pipe.

With it installed, a **RetroArch** row stands in the Games column, under Steam
— Steam is the row every session has, and a row that arrived with an install
must not push it down a place. Where RetroArch's own `.desktop` entry exists,
this row takes its place and that entry comes off the bar, exactly as the Steam
row takes the client's: two rows called RetroArch wearing one mark is the bar
saying the same thing twice, and of the two it is this one that leads to
somebody's games.

Pressing the row does whatever is left to be done, and never more than one thing
at a time:

1. **RetroArch is not installed.** The row says so, and the press asks whether
   to install it. Yes fetches the Flathub build into *this user's own* flatpak
   installation — no root, no polkit, nothing else on the machine touched — with
   a panel counting it up. A distribution package of RetroArch is preferred over
   the flatpak wherever both are present, because a native build can open a ROM
   wherever the user keeps one and a sandboxed one is limited to their home
   directory.
2. **No ROM folder has been chosen.** A panel asks where the games are, and says
   what the folder has to look like: **one subfolder per console, named after the
   console** — `ROMs/psp`, `ROMs/nes`, `ROMs/megadrive`. That name is the whole
   of what says which machine a game was written for, and no amount of reading
   the files can answer it. Choosing is a column of folders with **Select
   folder** standing over each one — the same picker as Settings > Games >
   RetroArch > ROMs path, which is where it is changed afterwards.
3. **Both done.** The press steps across to the **RetroArch** column, which
   stands immediately after Steam's.

The row that asks where the games are stands at the head of that column for as
long as the question is open — nobody has chosen a folder, or the one they chose
cannot be read this morning, or there is nothing in it yet — and goes the moment
there are games in it. After that the folder is a setting rather than a
question, and it is under Settings, where settings are.

That column is the folder as the shell reads it: the subfolders that hold
something become **consoles**, named as consoles rather than as folders where
the name is one this shell knows — `psp` is PlayStation Portable — and left
exactly as the user spelt them where it is not. A console with nothing in it is
not an empty column, it is not a column at all. Stepping into one shows the
games it holds, **as covers, on the layout a Steam library uses**, each under
its own name with the extension taken off. Games kept a folder each are found up
to three levels down and listed flat; the pieces of a disc image — the `.bin`
under a `.cue`, the discs a `.m3u` names — are not offered as games of their
own, because they are not games anybody can start.

**Choosing the folder also gives the emulator access to it**, and only to it. A
flatpak sees this user's home directory and nothing further, so a collection on
an external drive is one RetroArch starts and then cannot read — reporting it in
its own window, where nobody on a console is looking. The integration runs
`flatpak override --user --filesystem=<the folder>` for RetroArch alone at the
moment the folder is chosen: no password, no root, one application, one folder,
and `flatpak override --user --reset org.libretro.RetroArch` undoes it. A
distribution package of RetroArch is in no sandbox and nothing is done for it.

**A console needs a core, and the shell fetches it.** A core is the emulator
proper — RetroArch is the machine around it — and it comes in neither the
distribution's package nor the Flathub build, which ships none at all. So
choosing the ROM folder fetches one for every console found in it, from
libretro's own build server, which is where RetroArch's Online Updater gets
them and which needs no password and no root. They land in this user's own core
directory, the one RetroArch itself downloads into, so a core fetched here is
one RetroArch's own interface lists as installed — and one it fetched is one
this finds without being told. Which core is a table this integration carries,
best first; the first of a console's list that the server actually publishes is
the one taken.

Afterwards it asks. A game whose console has no core — a machine added to the
folder after the setup — answers its press with **"Get it and play?"**, and Yes
fetches that one core and starts the game as soon as it is there. That is the
only time anything is downloaded without being asked for, and it is the moment
somebody said "these are my games". A fetch that fails says so and starts
nothing; the panel over one can be put away, and the row under Games goes on
saying which core is coming down.

**And a core is not always the whole emulator.** Some of them are a shared
object *and* a folder of data they cannot run without: PPSSPP is 21 megabytes of
PSP emulator and none of the PSP's own fonts, so a core installed by itself
starts a game, draws every menu as a row of blank grey boxes, and says `Core
system files missing, expect bugs` along the bottom. Nothing is broken — the
half of the emulator that draws letters was simply never fetched. RetroArch
keeps those folders on a *System Files* page of its updater, separate from the
core downloader and reached from a different menu, which is how a machine ends
up with one and not the other.

So the shell fetches that too, from the same server, and a console whose core is
missing it reads exactly like a console with no core at all: the row says it
needs a download, and pressing a game gets the missing half. It is deliberately
a short list rather than a rule. The archives are named for the emulator and not
for the folder — Dolphin's folder is `dolphin-emu` and its archive is
`Dolphin.zip` — and most of what a core declares it needs is *not* published and
must not be: a Dreamcast core wants the Dreamcast's own boot ROM, which belongs
to whoever made the machine. The list is the part somebody checked by hand:
cores whose required files libretro publishes in full.

**Every console wears its own machine.** The column used to be a stack of
identical RetroArch marks, so nine consoles were nine rows you had to *read* —
and a glyph is read before a name is, and from further away. Each one is now the
machine itself, drawn in the shell's own material: the deck with the door on the
front, the brick with the corner cut off it, the cube, the cabinet, the keyboard
with the cartridge slot in it. A game with no cover wears its console's mark too,
so a shelf of PlayStation games still looks like PlayStation games.

They are drawn as the *object*, never as a logo or a controller — a wordmark is
illegible at the size a row is drawn and belongs to somebody else, and a
controller is the Games category's own mark and would say "a game" forty times
over. The drawings ship with `lxb-retroarch` rather than with the shell, because
a machine without that package has no console columns to put them on.

**Every game wears its cover, and stands the screen behind it.** A column of
forty identical marks says only how many files are in a folder; the cover says
which one each row *is*, from across a room, before the name has been read —
which is the whole reason the Steam column has one, and no reason a game somebody
dumped themselves should be the poor relation of one they bought. They come from
[libretro's own thumbnail collection](https://thumbnails.libretro.com), the same
one RetroArch fetches from: the box art becomes the cover, and the screenshot
stands behind the whole display while the cursor is on the row.

The screenshot is **deliberately blurred**, and that is not a stylistic
flourish. What libretro holds is a picture of a console's screen — three hundred
pixels tall — and a three-hundred-pixel picture enlarged across a television is a
wall of squares. Softened it is what it was always going to be: the colour and
the massing of the game, behind the row that is the game. See
`art::blurred_scenery_from`, which reduces it until there is no grid left to
enlarge and then softens what remains.

**A game does not have to be named the way the database names it.** That is the
whole trick and it is the reason RetroArch itself shows a hand-sorted collection
no artwork at all. libretro's names are the names of *dumps* —
`Tekken 6 (USA) (En,Fr,De,Es,It,Ru)` — and somebody who dumped their own disc
called the file `Tekken 6.iso`; asking for a picture by the file's name is a 404
every time. So the shell does not ask by the file's name. It fetches the listing
of the console's shelf once, keeps it, and matches every game in the folder
against it with the tags taken off both sides, the punctuation dropped, and a
database's `Legend of Zelda, The` put back the way the box says it. What is
deliberately *not* done is anything clever with numbers: turning roman numerals
into figures would make `Final Fantasy VII` meet `Final Fantasy 7` and would also
make `Mega Man X` meet `Mega Man 10`, and a wrong cover is worse than none
because a wrong one is not obviously wrong.

Where several dumps reduce to the same game — a PlayStation Portable shelf has
two `Tekken 6` and eleven `Tekken - Dark Resurrection` — one is chosen by whether
it is the game at all (a beta, a demo or a prototype loses, unless the file
itself says "demo"), then by region, then by the plainer name; and the same shelf
answers the same way every time, because a cover that changed between two runs
for no reason anybody could see would be worse than either of them.

Nothing about this is announced. The pictures are asked for once per folder per
session, only for what has not got them, and no panel stands over it: the covers
appearing one at a time down a column somebody is already scrolling *is* the
feedback. Everything lands in `$XDG_CACHE_HOME/lxb/retroarch-art`, in
libretro's own layout — never in RetroArch's thumbnail folder, because what
somebody sees in the emulator's own interface is the emulator's business.

**A game has a menu too:** play it, get its artwork, rename it, delete it. Get
the artwork is the row that has no counterpart elsewhere in the shell, and it is
there because the matching can miss — somebody who called their file `smb.nes`
has written down less than it needs. Which is why the row under it matters as
much: Rename is how a game gets called what it is, and the two together are the
answer to "why has this one no picture". Delete is offered on the terms a
photograph is and greyed on the same ones — a game outside the user's own home
directory is a file on somebody else's disk. **Uninstall is not on it at all**:
nothing installed a ROM, so the only thing that row could mean is deleting the
file, which the row below already says plainly. A press that finds no artwork
says so in a panel rather than leaving the row as it was.

**The row has a menu, like Steam's.** Raising it over RetroArch offers what can
be done to a collection rather than to a game: look through the games folder
again, fetch whatever emulators are missing and how many that is, change where
the games are, and — below the rule, because it is a different program — open
RetroArch's own interface, which the shell otherwise hides from its own category
and there would be no way back to. A machine that has not got RetroArch yet is
offered it and nothing else; a machine still waiting on the helper raises no menu
at all, because every row on it needs an answer that has not arrived.

**And the emulators have settings, under Settings > Games > RetroArch.** One page
per installed core at the top — PPSSPP's rendering resolution and texture
upscaling, Mesen's overclock — and under them the settings that belong to no core:
the aspect every game is drawn at, the driver it draws with, whole-number scaling,
whether it waits for the screen, where the games are, and a row that fetches every
game's artwork again — for the two reasons somebody would want that: libretro's
collection grows, and a game the shell could not put a name to has very often
been renamed since.

Not one of those core settings is written down in this repository, and that is the
point. A core *declares* what it can be set to — its keys, what to call them, the
groups it sorts them into, every value each will take — and it declares it to
whatever loads it. So the helper loads the core, asks it, and writes down the
answer; the page is whatever that emulator's authors put in it, in their order,
under their names, and it is right about a version of the core released after this
shell was. PPSSPP alone declares seventy-five settings across five groups. A
hand-written copy of that would be wrong the first time somebody updated the core
and would say so nowhere.

Starting a game is the shell's own launch, the one every row on this bar uses:
the loading screen, the display it is pinned to, and the guide's Close all work
on it exactly as they do on anything else, because what the helper answers with
is a command line and never a running process.

**And the controller is handed over with it.** An emulator is not like other
applications here: it binds *one device* to each player port, so which
controller lands on player one decides whether anything happens at all. On a
machine running this shell there is more than one of every pad — the guide
button is kept from applications by grabbing each controller and standing a copy
of it in the pad's place ([the guide button](controls.md#the-guide-button-is-the-shells-alone)), and Steam
mirrors every pad again as a virtual Xbox controller — so the first device an
emulator finds is usually the grabbed original, which by design says nothing to
anybody. That is a player one that cannot move.

So the shell works out the order itself, every single time a game starts: the
pad somebody last had their hands on leads, the other live ones follow, and the
grabbed originals go last. It is written as player indices into a small file of
its own and read on top of RetroArch's own settings with `--appendconfig`.

**And where the buttons are, for the pads RetroArch has never heard of.** An
emulator does not know one controller from another until it recognises it:
RetroArch keeps a list of the pads it has profiles for, and a pad that is not on
that list gets no buttons at all — every one of them dead, on whichever player
port it lands. Steam's own virtual controllers are not on that list, and this
shell puts one of those in front of a game every time somebody plays through
Steam Input, so a living room can very easily hold four controllers and nothing
that works.

The shell has a *different* list — SDL's, the one every game on this machine
already trusts, carried along with the controller reading it does anyway — and
it has the pads RetroArch's is missing. So for any pad that list knows, the
shell says where each control is, in the numbers RetroArch counts in: a button's
place in the device's own list of buttons, an axis's place in its list of axes,
and a D-pad named as a hat where the pad reports one. Naming the controls by
*place* rather than by the letter printed on them is what lets one answer cross
between an Xbox pad and a PlayStation pad, whose two middle face buttons are the
other way round.

**And for every XInput controller, whether either list has heard of it.** There
is always a pad newer than the lists — RetroArch's has no entry for an 8BitDo
Pro 3, and a controller nobody recognises is a controller with no buttons. But an
XInput pad is not a pad of unknown layout. It is a pad of *the* layout, the one
its driver has been required to send since the first Xbox controller, so a device
declaring that set of codes has already said where everything is. The shell reads
it straight off the device and needs no list at all.

Two codes make that worth stating carefully. The kernel calls `0x133` *north* and
`0x134` *west*, meaning the top button and the left one — and on a pad of Xbox's
shape that is exactly backwards, because its driver sent `0x133` for the X marked
on the left long before those names existed. A PlayStation pad sends the same two
codes the other way round and means the kernel's names by them. So the shell
insists on two things before reading a pad this way: analogue triggers on their
own axes, and no shoulder *buttons* beneath them, which is the pair no Sony pad
has ever matched. Anything failing that is left alone rather than guessed at.

Two things are deliberately left out. A pad **no** list knows and whose shape
cannot be read is given its player number and nothing else, leaving RetroArch to
answer for it exactly as it does today — a guess from a list is worth more than a
guess from this shell. And the **guide button is never handed over**: it is the
way out of whatever is in front, and an emulator with a binding for it would
answer the one press that is not an application's to answer.

**Steam Input is kept out of it.** Steam does not pass a controller through; it
takes the real one over and stands a virtual Xbox pad in front of it, so while
Steam runs every pad on the machine is on it twice and the copy answers to a
profile set for some other game entirely. An emulator binding one device per
player cannot prefer the real one, so the shell does it instead, and names only
as many player ports as it handed controllers over — a port past the end of that
list is a port RetroArch would fill by itself, out of exactly what was left out.
The exception is a controller Steam Input is the *only* driver for: there the
invented pad is not a duplicate but the whole of it, and leaving it out would
hand the game nothing at all. The second-generation Steam Controller used to be
that case and no longer is — the shell
[drives it itself](controls.md#the-pad-with-no-driver-at-all), so RetroArch is given the
real thing.

None of it is kept. RetroArch writes its settings back over its own
configuration when it closes and cannot tell a setting somebody chose from a
line appended on the way in, so the file switches that off for launches the
shell makes — otherwise one evening's pad order would stand as settings for
every launch afterwards, including the ones this shell knows nothing about. Save
files, save states, playlists and each core's own options are written elsewhere
and are untouched. Deleting the file loses nothing; the next game writes it
again.

