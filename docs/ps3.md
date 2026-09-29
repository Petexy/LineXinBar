# PlayStation 3

[Documentation](index.md) · [Project home](../README.md)

- [A package, built on RPCS3](#a-package-built-on-rpcs3)
- [The PlayStation 3 row](#the-playstation-3-row)
- [Where the games are](#where-the-games-are)
- [The PlayStation 3 column](#the-playstation-3-column)
- [The film and the music](#the-film-and-the-music)
- [Playing](#playing)
- [Controllers](#controllers)
- [Installing a package](#installing-a-package)
- [Uninstalling](#uninstalling)
- [Clearing a game's cache](#clearing-a-games-cache)
- [Encrypted discs](#encrypted-discs)
- [Trophies](#trophies)
- [Settings > Games > PlayStation 3](#settings--games--playstation-3)
- [What is RPCS3's and what is the shell's](#what-is-rpcs3s-and-what-is-the-shells)
- [When something does not work](#when-something-does-not-work)

## A package, built on RPCS3

RetroArch has no PlayStation 3 core, so PlayStation 3 games are played through
[RPCS3](https://rpcs3.net) and have a package of their own. Everything on this
page exists on a machine that has installed **`lxb-rpcs3`**, and on no other.
The shell looks for that program beside its own executable, then on `PATH`,
and looks again every five seconds, so installing or removing the package while
the session runs puts the row on the bar or takes it off. Without it, RPCS3 is
an application like any other. It is optional on the terms
[`lxb-retroarch`](retroarch.md) and [`lxb-heroic`](epic.md) are, and
version-locked to the shell, because the two talk over one JSON record per line
(`PROTOCOL` in `crates/lxb-rpcs3/src/report.rs`).

Any RPCS3 on the machine is used, and they are looked for in this order: a
distribution package (`rpcs3` on `PATH`), a flatpak in your own installation, a
flatpak installed for everyone, then an AppImage (one a desktop entry starts, or
an `rpcs3…AppImage` in `~/AppImages`, `~/Applications` or `~/Downloads`). The
flatpak comes before an AppImage because it is what the shell installs, and the
games it installs live inside the RPCS3 that installed them.

The mark on the row, the column and every game without an icon is RPCS3's "3",
from RPCS3's own icon and drawn anew on the shell's glyph grid
(`crates/lxb-rpcs3/glyphs/ps3.svg`), installed into `share/lxb/glyphs` with the
package. The helper carries the same drawing inside itself (`lxb-rpcs3 mark`),
and the shell asks it for the mark as it starts wherever no data directory has
one, so a helper run from where it was built, or a package installed without
its share folder, still draws the "3" rather than the Games pad. It is the one console mark that is a logo rather than the machine: the
PlayStation 3 here is RPCS3.

## The PlayStation 3 row

A **PlayStation 3** row stands in the Games column under Steam, Epic Games and
RetroArch, and RPCS3's own `.desktop` entry comes off the bar where it does.
RPCS3's window is one press away, as **Open RPCS3** in the row's menu. A press
does the one thing that is left:

1. **RPCS3, or the PlayStation 3's system software, is missing.** "Press to set
   it up". The press asks, and **Install** does both, with a bar: RPCS3's
   Flathub build into your own flatpaks (no password), then the console's
   system software from Sony's own update server, installed by RPCS3 itself.
   An RPCS3 that is already here only gets the system software.
2. **No games anywhere yet, and no folder known.** The press walks the disk
   for the folder your games are in.
3. **Otherwise** it steps into the PlayStation 3 column.

## Where the games are

Your PS3 folder is the folder chosen under Settings > Games > PlayStation 3 >
Games folder. Until you choose one, the PS3 folder inside RetroArch's ROM
folder is used, where RetroArch has one: a subfolder called `ps3`,
`playstation3`, `PlayStation 3` or `Sony - PlayStation 3`. While this package
is installed, RetroArch's column no longer lists that folder as a console of
its own, since RetroArch cannot play it.

Games are found up to three folders down:

| What | Looks like | How it is played |
| --- | --- | --- |
| A disc image | `Tekken 6 (USA).iso` | Booted straight from the image |
| A disc in a folder | `…/PS3_GAME/PARAM.SFO` | Booted from `PS3_GAME/USRDIR/EBOOT.BIN` |
| A game unpacked from a package | `…/PARAM.SFO` and `…/USRDIR/EBOOT.BIN` | Booted from `USRDIR/EBOOT.BIN` |
| A package | `something.pkg` | Installed on its first press, then played |
| A downloaded zip | a zip holding a `.pkg` and its `.rap` licence | Installed on its first press, then played |

The games RPCS3 has installed (`dev_hdd0/game`, category `HG`) are listed too.
A package whose game RPCS3 has already installed is not listed twice: the
installed game is the row. Nor is a game downloaded with its updates beside it
(`Super-Stardust-HD_Full.pkg` and `…_Update-6.00.pkg`): its packages are one
row, the earliest version's, and its press installs the game and then each
update in version order, under one bar.

## The PlayStation 3 column

The column is your games by name, each on its own **icon** (`ICON0.PNG`, drawn
on a card of its 320×176 shape). While a game is chosen, its **backdrop**
(`PIC1.PNG`) stands behind the whole display, as it did on the console. A game's
name is in the shell's language where the game has one in it (`TITLE_nn` in
its `PARAM.SFO`). A package is drawn without colour until it is installed.

Every picture is the game's own, read out of it. Nothing is fetched. Images and
packages are read in place, a zipped package included, and what they hold is
copied once into `~/.cache/lxb/rpcs3/art`. Folders are read where they are. A
game that has gone from the folder takes its pictures with it at the next scan.

The line under each game says what a press does: "Ready to play", "Installed",
"Press to install" or "Press to get it ready".

## The film and the music

On the console, a highlighted game played a short film (`ICON1.PAM`) in place
of its icon and its music (`SND0.AT3`) behind it. The shell does the same. Once
the cursor has rested on a game for a moment, its film plays on its card and its
music loops under it. Both stop the moment the cursor moves on, or a panel, the
guide, a launch or an application covers the start screen. The start screen's
own music steps aside while a game's plays. A game's music is played at the
shell's own level, and not at all where the shell is muted. Settings > Sounds >
Start music does not silence it: that switch is for the start screen's own
music, and a game's music is part of the game, as its film is.

A game with no film keeps its icon, and a game with no music plays in silence.
Many games have neither: of the three disc games this was first tried on, none
did. Super Stardust HD, from the PlayStation Store, has both.

## Playing

A game starts through the shell's ordinary launcher, so it gets the loading
screen, the display it was started on and the guide's Close. The loading screen
stands the game's `PIC0.PNG` in the middle of its backdrop, the way a Steam
game's stands its logo. RPCS3 is started without its own window, full screen:
`rpcs3 --no-gui --fullscreen --input-config lxb <game>`.

The first start of a game is slow: RPCS3 compiles the game's code and shaders
before it plays, behind its own progress screen. Later starts reuse what it
compiled.

The Flathub build of RPCS3 asks "Do you wish to use this build anyway?" on every
start, beside the game, with No (which quits the game) as its default and no way
to stop asking. It asks because `FLATPAK_ID` is set, so a game started from the
bar has that variable taken away inside the sandbox
(`flatpak run --command=env net.rpcs3.RPCS3 -u FLATPAK_ID rpcs3 …`;
`--unset-env` cannot do it, because flatpak sets the variable after applying
it). RPCS3's own window, from Open RPCS3, still asks.

Two more of RPCS3's questions would stop a game, and are turned off in its
window settings (`GuiConfigs/CurrentSettings.ini`, `[main_window]`) before
every game: `confirmationBoxExitGame`, the "Exit Game?" RPCS3 asks when the
game's window is closed, which is what the guide's Close does, and
`infoBoxEnabledWelcome`, its first-start welcome, which a game started without
RPCS3's window still puts up, and which quits the game when it is closed. Both
stay off for RPCS3's own window too.

## Controllers

A fresh RPCS3 gives no player any controller. So the moment before a game
starts, the shell writes RPCS3's controller file,
`input_configs/global/lxb.yml` in RPCS3's configuration, in the order it hands
controllers to RetroArch: the pad in your hand first, then the others. Each
player reads its pad through SDL under SDL's name for it (`<name> <n>`). The PS
button is Select and Start together, because the guide button never reaches an
application. The others are where Settings > Games > PlayStation 3 >
Controller buttons puts them, which is RPCS3's own default until something is
moved.

Every pad the shell guards is on the machine twice, the silent original and a
working copy under the same name (see [Controls](controls.md)). The launch puts
the copies first in `SDL_JOYSTICK_DEVICE`, so SDL opens them before the
originals and each copy is the first of its name. RPCS3's log says which device
it bound for each player (`SDL: Found game pad … path=…`, then `Pad 0:
device=…`).

## Installing a package

A `.pkg`, or a zip with one in it, is installed by **RPCS3's own installer**
(`--headless --installpkg`). It knows which update fits which game and where an
add-on goes. The shell adds the parts it does not do:

- A zipped package is taken out of its zip into a scratch folder RPCS3 can read
  (for the flatpak, its own cache), and deleted once installed. Your zip is not
  touched.
- The `.rap` licence, from the zip or beside a loose package, is copied into
  RPCS3's `dev_hdd0/home/<user>/exdata`, as RPCS3's own window does.
- A bar, read off the disk as the game's folder fills.

There are two ways in: press a package's row in the PlayStation 3 column, or
press a `.pkg`, or a zip holding one, in Files. The Files route asks "Install this
PlayStation 3 game?" instead of unpacking the zip, and says when it is done
that the game is in the PlayStation 3 column. The package file stays where it is.

## Uninstalling

A game RPCS3 installed has **Uninstall** in its menu, while no PlayStation 3
game is playing and nothing is installing. It asks first, with the game's size
and "Its saved games and trophies stay.", and Keep It is the answer stood on.

What goes is what RPCS3's own "Remove game" takes: the game's folder under
`dev_hdd0/game` (the game, and any update or add-on installed into it), its lock
file in `dev_hdd0/game/＄locks`, what RPCS3 compiled for it (`cache/<serial>`
in RPCS3's cache) and its system cache (`dev_hdd1/caches/<serial>_…`). What
stays is what a console keeps when a game is deleted: its saves, its trophies,
its licence, and any settings of RPCS3's own made for it. The package it came
from is not touched, so a package in your PS3 folder is back in the column as
"Press to install".

The helper removes only a folder straight under RPCS3's `dev_hdd0/game` that
says it is an installed game (`PARAM.SFO`, category `HG`) with a serial of the
console's shape. The serial is what the caches are matched by.

## Clearing a game's cache

RPCS3 compiles each game's code and shaders the first time it plays, and keeps
them for the next start, under `cache/<serial>` in its cache folder, with the
game's own system cache in `dev_hdd1/caches/<serial>_…`. For some games that is
gigabytes. Where a game has any, its menu has **Clear cache**. It asks first,
with how much it is and "The next start will take longer while it is prepared
again.", and Not now is the answer stood on. The game itself, its saves and
everything else stay. The size is read again every time a game ends, so the
entry is there once a game has been played.

## Encrypted discs

A disc image straight from a Redump dump is still encrypted, and RPCS3 boots
one only with the disc's key in a file named after the image. When a game needs
one, its press fetches it first ("Getting the game ready") and then starts the
game. The key comes from Redump's published collection of PlayStation 3 disc
keys, as mirrored by the PlayStation 3 IRD database. It is saved as
`data/redump/<image name>.key` in RPCS3's configuration, never beside your
games.

A key is proven, never matched by name: it is kept only if it decrypts the
disc's own `LIC.DAT` (or `EBOOT.BIN`) to the bytes that file starts with. That
matters twice over. Dumps are renamed (Redump calls one of the discs this was
tried on `Skate 3 (USA, Asia) (En,Fr,Es)`, and the file was `Skate 3 (USA)
(En,Fr,Es).iso`), and the right key is found anyway. And an image somebody has
already decrypted keeps Redump's region table, so it looks encrypted from the
outside. RPCS3 applies a key it finds by name without checking it, so a key for
one of those would make noise of the whole game. Such an image is never given a
key.

## Trophies

Every game's trophies are in the Trophies column beside Steam's, Epic's and
RetroAchievements', read only. The trophies themselves come out of the game's
own `TROPHY.TRP`, in your language where the set has it (`TROP_nn.SFM`). What
is unlocked, and when, comes from RPCS3's record of the set
(`dev_hdd0/home/<user>/trophy/<set>/TROPUSR.DAT`), which exists once the game
has registered its set. That happens at the game's own moment: Super Stardust HD
does it 27 seconds in, and Skate 3 only after its first-start "install game data"
question has been answered. Until then every trophy is locked, as on a console
that has not played the game. RPCS3's copy of a set keeps only one language
(its own), so it is read on its own only where the game can no longer be read.

They are listed as Steam's achievements are, in three sections: Unlocked,
Locked, and Hidden at the bottom, each with its count. A hidden trophy is never
among the locked ones. Until it is earned it reads "Hidden achievement" and
"Press to reveal details", and the press shows its name, description, grade
and state. Each trophy's grade and state are on its row. A locked
trophy's picture is grey, as a locked Steam, Epic or RetroAchievements one is: a
set carries its pictures only in colour, so the helper makes the grey copy once
and keeps it beside them in `~/.cache/lxb/rpcs3/trophies/<set>`. They are read again after
every scan of the games, and whenever a game RPCS3 was playing ends.

## Settings > Games > PlayStation 3

**Games folder** is where your PlayStation 3 games are. It is the same row that
stands at the head of the column while the question is open.

Once RPCS3 is here, the page has its settings too. Each is written into RPCS3's
own `config.yml`, the file its Settings window writes, and read back from there
whenever the page is built, so the two never disagree and the shell keeps no
copy (`crates/lxb-desktop/src/ps3_settings.rs`):

| Row | Values | RPCS3's setting |
| --- | --- | --- |
| Resolution | 720p (as the console had it), 1080p, 1440p, 4K | `Video: Resolution Scale` 100, 150, 200, 300 |
| Aspect ratio | 16:9, 4:3, Fill the screen | `Video: Aspect ratio`, and `Stretch To Display Area` for the last |
| Video driver | Vulkan, OpenGL | `Video: Renderer` |
| Frame rate limit | Automatic, 30, 60, Off | `Video: Frame limit` |
| Wait for the screen | On, Off | `Video: VSync Mode` Full or Disabled |
| Performance overlay | On, Off | `Video: Performance Overlay: Enabled` |
| Console language | the PS3's twenty, each in its own words | `System: Language` |
| Trophy notices | On, Off | `Miscellaneous: Show trophy popups` |

A value set in RPCS3's own window that is not on a list shows as what the file
says. A game's own settings in RPCS3 (its custom configuration) still win over
these for that game, as they do in RPCS3.

**Console language** follows the shell's language until one is chosen here:
before every game the console is set to the nearest language a PS3 had (British
English for Hindi). Choosing one stops that, and `ps3-language-chosen` in
`shell.toml` remembers it.

**Controller buttons** is the shell's own, because the shell writes RPCS3's
controller file itself before every game (see [Controllers](#controllers)).
There is one page per PS3 button (Cross, Circle, Square, Triangle, L1, R1, L2,
R2, L3, R3, Start, Select), each listing the pad's buttons by where they are,
and the choice goes to every player. **Put every button back** returns each to
where a PS3 controller has it, which is also RPCS3's default. Only the buttons
that were moved are kept, under `[ps3-buttons]` in `shell.toml`. The PS button
stays Select and Start together, and the D-pad and sticks are not moved.

## What is RPCS3's and what is the shell's

RPCS3 owns the console: its system software, its hard disk (`dev_hdd0`), its
installed games, saves, licences, trophies, keys and every emulation setting.
Open RPCS3 from the row's menu to change any of them. The shell writes into
RPCS3's configuration only what follows: `input_configs/global/lxb.yml`, the
two questions turned off in `GuiConfigs/CurrentSettings.ini` (see
[Playing](#playing)), the settings on its [page](#settings--games--playstation-3)
in `config.yml` when they are changed there (and the console's language before
a game, until one is chosen), a disc key under `data/redump`, and a licence
copied into `exdata` with its package. It removes from it only what
[Uninstall](#uninstalling) and [Clear cache](#clearing-a-games-cache) take, and
only when they are pressed. It keeps
its own copies of the games' pictures in its cache.

The helper talks to the network for three things only, each when something
you did asks for it: Flathub (installing RPCS3), Sony's update server (the
system software, over plain HTTP, which is the only way Sony serves it; RPCS3
checks the file's own digests before installing it), and GitHub (Redump's key
collection, only for an image that needs a key).

## When something does not work

The session log carries everything the helper said, and RPCS3 keeps its own log
at `~/.var/app/net.rpcs3.RPCS3/cache/rpcs3/RPCS3.log` for the flatpak (or
`~/.cache/rpcs3/RPCS3.log`). The helper can be asked the same questions by hand:

```
lxb-rpcs3 probe                      which RPCS3, and its system software
lxb-rpcs3 scan ~/Games/ps3           the games, as the column lists them
lxb-rpcs3 key ~/Games/ps3/game.iso   whether a disc needs a key, and fetch it
lxb-rpcs3 trophies ~/Games/ps3       every set, as the Trophies column reads them
```

(`lxb-rpcs3 remove FOLDER` is Uninstall and `lxb-rpcs3 clear-cache SERIAL` is
Clear cache; neither asks first.)

`LXB_RPCS3_IGNORE_APPIMAGE=1` makes the helper look past AppImages, which is how
the first-time setup can be tried on a machine that keeps one.
