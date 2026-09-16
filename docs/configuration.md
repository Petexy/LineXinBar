# Configuration

LineXinBar reads `$XDG_CONFIG_HOME/lxb/config.toml`, falling back to
`~/.config/lxb/config.toml`. A missing file is not an error: the defaults
are a usable single-display setup.

Pass `--config PATH` to use a different file.

Unknown keys are rejected rather than ignored, so a typo is reported at
startup instead of silently doing nothing.

The shell keeps its own file beside this one — see
[Per-application settings](#per-application-settings) at the foot of this page.
It is written by the shell rather than read from it, and is not part of
`config.toml`.

## `[general]`

| Key             | Type              | Default            | Meaning |
| --------------- | ----------------- | ------------------ | ------- |
| `autostart`     | list of strings   | `[]`               | Commands run once the private XWayland server is ready, or immediately in Wayland-only fallback mode. |
| `shell`         | string            | `"lxb-desktop"`    | The session shell started by `--shell`. Consulted only when that flag is given. |
| `output_layout` | `horizontal` \| `vertical` \| `mirror` | `horizontal` | How outputs without an explicit `position` are arranged. |
| `output_gap`    | integer           | `0`                | Logical pixels inserted between auto-placed outputs. |
| `background`    | `[r, g, b, a]`    | `[0.02, 0.02, 0.04, 1.0]` | Colour behind everything, components in `0.0..=1.0`. |
| `draw_cursor`   | boolean           | `true`             | Draw the compositor's own cursor when there is one to draw. Turn off when nesting inside a compositor that already draws one. |
| `cursor_theme`  | string            | *(unset)*          | XCursor theme for the pointer. Unset falls back to an inherited `XCURSOR_THEME`, then to the bundled Bibata Modern Classic. |
| `cursor_size`   | integer           | *(unset)*          | Nominal cursor size in logical pixels. Unset falls back to `XCURSOR_SIZE`, then 24. |
| `env`           | table of strings  | `{}`               | Environment variables exported to every child process. |

`mirror` puts every output at the origin, so they all show the same region.

`background` is what is behind everything **when there is no session shell**.
A `--shell` session never shows it: the compositor draws the shell's own
analytic wallpaper into every frame that has no session content in it, which
is the interval before the shell's first frame and the interval after its
last. Both used to be this colour, and this colour is very nearly black — a
login screen handing over to a black screen and a logout starting with one.
The palette comes from `~/.config/lxb/shell.toml`, or from a display manager
that hands the session its wallpaper clock; see `LXB_BACKGROUND_HANDOFF`
below. So does the material it is drawn in — the `theme-wallpaper` key, below
that.

The resolved cursor theme, size, and search path are exported as
`XCURSOR_THEME`, `XCURSOR_SIZE`, and `XCURSOR_PATH` to every child process, so
applications drawing their own pointer match the compositor's. LineXinBar ships
a subset of [Bibata Modern Classic](https://github.com/ful1e5/Bibata_Cursor)
under `share/icons/` (found relative to the binary in both a build tree and an
installed prefix), and compiles the default arrow into the binary as a last
resort, so a missing theme can never leave the pointer invisible.

Whether the pointer is on screen at all is a separate question, and not one
this setting answers: the session starts without a cursor and shows one only
once something moves it, hiding it again whenever a key or a controller button
is pressed. See [The cursor](../README.md#the-cursor).

`shell` differs from `autostart` in that LineXinBar supervises it: the session
ends when it exits, and failing to start it is fatal rather than leaving a
compositor with nothing on screen. A bare program name is looked for next to
the `lxb` binary first, so a build tree runs its own matching shell.

`WAYLAND_DISPLAY`, `WAYLAND_SOCKET`, `DISPLAY`, and
`LXB_XWAYLAND_DISPLAY` are session-boundary variables rather than
configurable child environment. After applying `general.env`, LineXinBar points
`WAYLAND_DISPLAY` at its own socket, removes `WAYLAND_SOCKET`, and either
removes `DISPLAY` or replaces it with its private XWayland display. This keeps
a nested session from leaking clients to its host.

LineXinBar also identifies children as a Wayland session through
`XDG_SESSION_TYPE=wayland`, `XDG_CURRENT_DESKTOP=LineXinBar`,
`XDG_SESSION_DESKTOP=LineXinBar`, and `DESKTOP_SESSION=lxb`. Activation
tokens inherited from the outer compositor are removed because they are not
valid in the inner session.

D-Bus is a separate activation boundary, and the same names have to reach it:
a service the bus starts on demand inherits nothing from whatever asked for it,
so a bus that has not been told what this session is starts the desktop portal
with no display and no desktop name — which is a session with no screen sharing
and no file chooser in it, silently. LineXinBar therefore replaces the bus daemon's activation
environment (and the systemd user manager's) once its Wayland and XWayland
sockets are ready, on a session that **owns the seat** — the DRM backend, which
is the machine's own session — and stops the portal again on its way out so the
next session starts one of its own.

Nested inside another desktop the host's bus is not LineXinBar's to rewrite.
Run the complete session through `env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u
DISPLAY LXB_PRIVATE_DBUS=1 dbus-run-session -- lxb --shell` (the nested helper
already does); the marker says the session has a bus of its own, and only then
is that bus's environment replaced. Without it, a nested session leaves the
host's activation environment — and the host's portal — alone.

### `LXB_BACKGROUND_HANDOFF`

Not a setting: a one-shot record a display manager may put in the session's
environment, saying which wallpaper was on screen when it handed over and how
far its clock had run. A session started without one still comes up on the
wallpaper — the palette from `shell.toml`, the clock from zero — so nothing
requires it.

It is read twice, and by design. The compositor reads it for the frames it
draws before the shell exists (and after it has gone), then passes the record
through **unaltered** to the session shell alone, which does the whole of its
validation: that it names a visual this shell draws, that it was sampled on
this boot, and that it is not stale. Nothing else in the session sees it —
neither XWayland, nor autostarts, nor the portal, nor an application launched
later — and an inherited copy is removed at every one of those boundaries.

The record is `key=value` pairs joined by `;`, ASCII, at most 1024 bytes:

```text
v=1;visual=lxb-wallpaper-v2;clock=linux-monotonic;boot=<boot id>;sample-ns=<n>;scene-ns=<n>;accent=<palette>[;theme=<Default|Simple>]
```

`sample-ns` is `CLOCK_MONOTONIC` when the record was written and `scene-ns` is
the wallpaper clock at that instant, so the far end advances one by the
difference to recover the other. `boot` is `/proc/sys/kernel/random/boot_id`,
which is what makes a monotonic sample meaningful to another process. `accent`
is one of the palette names Settings offers.

`theme` is the one optional field, and the one field whose absence means
something: it names the material the wallpaper was being drawn in — see the
`theme-wallpaper` key below — for the benefit of a reader that cannot consult the
account's own settings. Only the wallpaper's half of that setting; the marks the
shell draws are no part of a wallpaper, so the reader this is written for has no
use for them and the record has never carried them. The compositor drawing a bridge frame in front of a *login screen*
is that reader: it runs as the greeter's own account, and the settings of the
person whose wallpaper is on the screen are in somebody else's home directory.
Where the field is absent, or names a material this build does not have, the
reader falls back to the settings it can read. The session shell ignores it
outright — the shell is the account and has already read its own file — but it
accepts a record carrying one rather than refusing the phase over a field that
is none of its business.

Bump `visual` if the wallpaper
changes in a way that would draw a different frame at the same clock:
LineXinBar's own consumer refuses a visual it does not know, which is a
session that starts its animation from zero rather than one that jumps.

Console Experience Desktop Manager writes it. Any display manager can.

### `LXB_HOLD_DISPLAY`

Also not a setting. Set to `1`, `yes` or `true` by a display manager on both
sides of a login, it says that the displays are being passed between
compositors rather than given back to a console. Anything else, the variable
being absent included, means a session that ends by handing the machine back to
whoever started it.

It changes three things, all of them at the edges of a session and none of them
while one is running:

* **Coming up, the device is inherited rather than reset.** A compositor
  normally disables every connector and clears every plane on opening the GPU,
  so that no earlier compositor's state can make its own commits fail. That is a
  black screen for as long as it takes to reach the first frame — a fifth of a
  second here — laid over whatever the last compositor was still showing.
  Inheriting instead means the connector-to-CRTC mapping is recovered as it was
  left, so the first commit is a plane update on a live display rather than a
  modeset. If what was inherited turns out to be unusable, the device is reset
  and rescanned once, and the flicker comes back rather than the display
  staying dark.
* **Going down, the colour pipeline is left alone.** Undoing HDR makes the panel
  re-sync, which is a black screen of the display's own making arriving exactly
  where one is being removed. The compositor that follows sets its own within a
  frame. This is the same decision as keeping the night light across a
  hand-over, applied to the rest of the pipeline — so a display manager that
  sets this must be handing to a compositor that sets its own colour state, or
  the next session inherits BT.2020/PQ and does not know it.
* **Going down, the picture is held.** Closing the last handle on a DRM device
  is itself what blanks a display: the kernel destroys the framebuffers that
  file created, and removing one a plane is still scanning out disables the
  plane and the CRTC behind it. So a child process is forked to hold that
  descriptor open, and does nothing else with it, until the next compositor has
  committed a frame of its own or ten seconds have passed. Nothing is drawn, no
  device is held open beyond that, and a session that fails to start therefore
  leaves a machine that plainly needs attention rather than one frozen on a
  picture of a login screen.

Set it for a hand-over and only for a hand-over. On a bare TTY it would hold a
picture over the console a user is expecting back.

Holding the picture is necessary and is not sufficient. What the compositor that
follows does with the display it was handed decides whether anybody sees a black
screen — see below.

## `theme-wallpaper` and `theme-icons`, in `shell.toml`

`~/.config/lxb/shell.toml`, top level, beside `accent`:

```toml
accent = "Purple"
theme-wallpaper = "Default"
theme-icons = "Default"
```

How much material each half of the shell is drawn with. `Default` is its own
look: the wallpaper's current is a band of water three sheets thick, lit as
bodies, and every one of the shell's own marks is a bead of water shaded out of
its own distance field. `Simple` stands that down — the current becomes the three
fine glass-silk ribbons the shell drew before the band, and a mark becomes the
flat shape of itself in white, tinted with the accent. The *drawings* do not
change; only what they are made of does.

Two keys rather than one, and either may be either way round. They are separate
settings because they are separate expenses and separate tastes: the wallpaper is
one evaluation of a long function for every pixel of every screen on every frame,
and a mark is a few dozen pixels of a settings row. Measured on an RX 9060 XT,
one full-screen evaluation of the wallpaper is **0.38 ms** at 1080p in `Default`
and **0.20 ms** in `Simple`, and a mark goes from six reads of its distance field
to one. A machine that cannot pay for the first can very well pay for the second.

They are written from Settings > Appearance > Theme, which is a page with a row
for each. An unknown name is read as `Default`.

A file written before the setting was split carries a single `theme` key, which
said one thing about the whole shell. It is still read, and read as what it
meant — both halves take it where they have no key of their own — so a machine
deliberately stood down to `Simple` stays there across an update. It is never
written: the first save afterwards replaces it with the pair.

Both halves of a hand-over read these keys. The compositor reads
`theme-wallpaper` for the bridge frame it draws before the shell's first frame —
that frame is a wallpaper and nothing else, so `theme-icons` is none of its
business — and the display manager reads both for the login screen, which draws
its own marks. So a machine set to `Simple` is in `Simple` from the moment the
greeter appears, and never changes material in front of the user. A greeter
drawing for somebody else's account passes the wallpaper's answer along in the
hand-over record; see `theme` under `LXB_BACKGROUND_HANDOFF`.

## `retroarch-roms`, in `shell.toml`

`~/.config/lxb/shell.toml`, top level, beside the keys above:

```toml
retroarch-roms = "/home/someone/ROMs"
```

Where the RetroArch integration looks for games. Read by nothing unless the
optional `lxb-retroarch` package is installed — on every other machine it is a
line in the file that nothing opens, which is deliberate: uninstalling the
integration and putting it back should not lose the answer.

The folder holds **one subfolder per console, named after the console**, and
that name is the whole of what says which machine a game was written for:

```text
ROMs/
  psp/Tekken 6.iso
  nes/Metroid.nes
  megadrive/Sonic.md
```

`psp`, `PSP`, `playstationportable` and forty other console names are
recognised; a folder whose name is not, becomes a column under its own name and
lists what is in it anyway. A subfolder with no game in it is not listed at all.
Games kept a folder each — a `.cue` and its `.bin`, a `.m3u` and its discs — are
found up to three levels down and listed under their own names; the pieces of a
disc image are not offered as games of their own.

It is written from Settings > Games > RetroArch > ROMs path, and from the head
of the RetroArch column, which asks for it until there is a folder with games in
it and then stops asking. Both open the same picker: a column of folders, with
`Select folder` standing over each one.

Choosing a folder also runs `flatpak override --user --filesystem=<folder>` for
`org.libretro.RetroArch`, and only where RetroArch is a flatpak of this user's
own. Without it, a collection anywhere outside the home directory is one a
sandboxed RetroArch starts and cannot read. It needs no authority of any kind,
touches one application, and is undone with
`flatpak override --user --reset org.libretro.RetroArch`. Editing this key by
hand does not do it; the folder is granted when it is chosen.

Which core runs a console is not configurable here. The integration carries a
table of them, best first, and takes whichever of a console's cores is actually
installed.

When none is, it fetches one — from `buildbot.libretro.com`, where RetroArch's
own Online Updater gets them, into `~/.var/app/org.libretro.RetroArch/config/
retroarch/cores` for a flatpak and `~/.config/retroarch/cores` for a
distribution package. Both are RetroArch's own core directory, so nothing here
keeps a second collection of cores beside the one RetroArch knows about.

The controllers are not configurable here either. Every time a game starts, the
shell writes `lxb-controllers.cfg` beside RetroArch's own `retroarch.cfg` and
hands it to RetroArch with `--appendconfig`. It holds one line per player —
`input_playerN_joypad_index` — putting the pad somebody last touched on player
one, the other live pads after it, and last of all the pads this shell has
grabbed the guide button of, which cannot answer an emulator at all.

The controllers Steam Input invents are not among them. Steam stands a virtual
Xbox pad in front of every controller it takes over, and that copy answers to
whatever profile Steam has for some other game; the real pad is the one worth
binding. The file also sets `input_max_users` to the number of controllers it
actually handed over, so a player port past the end of that list cannot be
filled by RetroArch out of what was left out. A controller Steam Input is the
only driver for is still handed over, because leaving it out would hand the
game nothing. The second-generation Steam Controller is no longer one of those:
the kernel gives it no driver, so the shell builds its gamepad itself and
RetroArch is handed that.

It also holds that pad's buttons — `input_playerN_b_btn`,
`input_playerN_l2_axis`, `input_playerN_up_btn` and the rest — because
RetroArch's own list of pads does not have Steam's virtual controllers on it,
nor any controller newer than the list, and a pad it does not recognise has no
working buttons at all. Those numbers come from SDL's mapping database for the
pads it knows, and otherwise from the device's own declaration for any pad of
Xbox's shape, which needs no database: an XInput controller sends one fixed set
of codes and always has. A pad that is neither is given its player number alone,
which leaves RetroArch's own guess in place. The guide button is never written
for any pad.

Nothing in the file is kept. It ends RetroArch's *save settings on exit* for
that launch, because RetroArch cannot tell a setting somebody chose from a line
appended on the way in, and one evening's pad order is not a setting. Save
files, save states, playlists and each core's own options live elsewhere and are
unaffected; what is given up is a setting changed inside RetroArch during a game
the shell started. Deleting the file loses nothing; the next game writes it
again.

Some cores also need a folder of their own beside them — PPSSPP's fonts and
atlases, Dolphin's `Sys` tree — and that is fetched with the core, into
RetroArch's `system_directory` (its own setting, read where it has been moved,
and a `system` folder beside the configuration otherwise). A console whose core
is installed but missing that folder counts as a console with no core: the row
says it needs a download, and the press fetches only the part that is missing.
Nothing here fetches a console's own BIOS, which is not libretro's to publish.

Under **Settings > Games > RetroArch** are the emulators' own settings, one page
per installed core, and under those the settings belonging to no core — aspect
ratio, video driver, whole-number scaling, waiting for the screen. Nothing on a
core's page is written down in this shell: the core is loaded and asked what it
can be set to, so the page is that emulator's own list under its own names. A
chosen value is written where RetroArch reads it — `config/<Core>/<Core>.opt`
for a core's own settings, `retroarch.cfg` for the rest — and it stays there,
unlike the controller order, which is appended for one launch only. Both files
are rewritten whole, so nothing else in them is disturbed.

Cores happen without asking exactly once: when the ROM folder is chosen, for
every console in it. After that a console with no core asks first, on the press of one
of its games. Neither is configurable, and a machine that should never fetch one
can leave the folder unset — or not install the package, which is the setting
that turns all of this off.

## Which core plays a console

Not a setting. `crates/lxb-retroarch/src/consoles.rs` lists the cores that can
play each console, best first, and that order is kept — less whatever this
machine has already downloaded and found it could not open.

It was not always kept. A core that could start without firmware was once sorted
in front of one that could not, which on a machine with no PlayStation 2 BIOS put
`play` ahead of `pcsx2`. Play! needs no BIOS and starts; it also runs very little.
Compatibility is what a console is chosen for, and the answer to a missing BIOS
is to ask for the BIOS — see **A missing BIOS** below.

What a core needs is never written down in this repository. libretro publishes
it per core in the `<core>_libretro.info` files that ship with RetroArch, and
those are read instead — see `crates/lxb-retroarch/src/firmware.rs`, which also
explains why a table of BIOS file names here would be wrong within a year.

### Installing it

flatpak installs a runtime, its extensions and the application as separate jobs,
each with a progress bar of its own that starts again at nothing — so a panel
showing that percentage filled and emptied three or four times over with one
sentence under it, which reads as an install that keeps failing. The helper reads
the `Installing n/m` flatpak prints beside the bar and makes one bar of the
whole transaction, so it only ever goes forwards, and says "Getting what
RetroArch needs" until the last job, which is RetroArch itself.

flatpak is run under `LC_ALL=C` for that: the word in front of those numbers is
translated, and a progress bar that worked in some countries and not others
would be worse than none.

### What a fresh RetroArch is set to

One setting, written once, and only into a configuration nothing has ever
chosen for: **`video_driver = "vulkan"`**.

RetroArch's own default is OpenGL. It was the right default for the machines it
was chosen on and it is the wrong one here — a machine running this shell is
drawing its own bar through Vulkan or it would not have got this far. Left on
OpenGL, RetroArch comes up on the GL driver, discovers at content load that the
core wants a Vulkan context, overrides itself for that session and restores
OpenGL on the way out, so it does the same dance on every launch.

**Only where the line is absent.** A `retroarch.cfg` with no `video_driver` in
it is one nobody has chosen for. RetroArch writes every key it has on the way
out, so the moment it has been run once — or somebody picks a driver under
Settings → Games → RetroArch → Video driver — the line is there and this never
looks again. That is what makes it a default rather than the shell overruling
people.

**And only where Vulkan is actually here.** The test is that something already
uses it: the shell opens its own renderer for Vulkan or OpenGL and takes
whichever it gets, so a machine with no Vulkan answers no and nothing tells an
emulator to use a driver that is not there. Nothing is probed for it — a
separate look at what the machine supports would be a second answer to a
question already answered, and it could disagree with the one the shell is
running on.

It is applied on the first frame where both facts are known: that there is a
RetroArch, which the probe says, and which backend the shell drew through, which
does not exist until the first frame. A removal puts it back to being looked at
again, because what comes back after that is a fresh machine.

### Its own interface

The menu over the RetroArch row under Games — **Open RetroArch**, below the
rule — starts the emulator's own screens: its command line and nothing else, no
core and no game. RetroArch's desktop entry is hidden from the applications on
the bar, so this row is the only way to those screens from the shell, and it
joins the launched applications under its own name so the guide can close it
like anything else.

It is answered on the screen exactly as a game or a tile is: the loading screen
grows out of the row wearing the emulator's mark, holds the display until the
window is there, and a second press while it is up is spent. A flatpak takes a
few seconds to come up, and for that stretch the press used to be answered by
nothing at all — the menu folded away and the bar sat there.

### Taking it off again

The same menu — **Remove RetroArch**, below the rule with *Open RetroArch*, and
last of the three because it is the only row there that takes something away.
It asks first, and the question names what goes.

What goes is the application and everything kept for it:

* the flatpak itself, `flatpak uninstall --user --delete-data`, which takes
  `~/.var/app/org.libretro.RetroArch` with it — RetroArch's own configuration,
  every core this shell downloaded into it, the assets, the playlists, the save
  files, and any BIOS that was put there;
* the filesystem permission the shell granted for the games folder, which
  survives an uninstall otherwise and would be silently inherited by a
  reinstall;
* this shell's own cache of the cover art fetched from libretro, which lives
  under the *shell's* cache rather than RetroArch's and so is out of
  `--delete-data`'s reach;
* the scratch directory a core's files land in while it is being asked what it
  can be set to;
* and the one line in the shell's own settings that belongs to this integration
  and nothing else: where the games are.

**The games are not touched**, and neither is the folder they are in. This
removes a program and what the program kept.

Everything the shell had read off it goes at the same moment, and that is not
tidying up: the shelf of games, the emulators' settings pages and the firmware
rows are all built out of the last scan and the last ask, so leaving them
standing would draw a column of games nothing can play and settings pages for
emulators that are no longer installed. The row then asks the machine again, and
comes back offering to download it — which is where a fresh machine starts.

**Only the `--user` flatpak.** That is the one this shell installs and the only
one it can remove without a password: a system-wide flatpak's uninstall needs
root and a distribution package needs the package manager. Both are somebody
else's decision to undo, and the panel says so rather than raising a password
prompt over a television.

The row exists because the setup is a sequence of first-time questions — where
the games are, which cores to fetch, where a BIOS is — and every one of them can
otherwise only be seen once per machine.

### A missing BIOS

Several consoles have no software of their own until the machine's boot ROM is
there. That file belongs to whoever made the console, nobody may redistribute
it, and the only lawful copy is one dumped from hardware somebody owns — so this
integration will never fetch one and RetroArch's own updater will not either.

What it does instead is **ask where yours is** — after the game has failed to
start, and never before. Press a game and it is started. If the emulator comes
straight back without playing anything, and there is a BIOS for that console
nobody has chosen, a panel names the game, says it did not start, and offers to
choose a folder. What is chosen is copied — not read from where it lies —
because RetroArch has no setting for it: its cores look for particular names
below one system folder of its own, so pointing an emulator at a downloads
directory is not a thing the format allows.

**The question used to stand in front of the press**, on any console whose core
declared a file this machine had not got, and that was the shell deciding in
advance what an emulator would do with it. It is wrong often enough to matter:
melonDS plays most Nintendo DS games with its own high-level BIOS and declares
all eight of its files optional, and half the cores that name a boot ROM name
three regions of it. A game that would have run perfectly well was answered with
a question instead. Whether a console can manage is the emulator's business, and
the emulator answers by running.

So nothing on a game's row says a BIOS is missing, either. It says which console
the game is for, and — where nothing on the machine can play it at all — that a
core is a download away. That second one really is known before the press.

Two things have to be true before a failed launch is answered: it failed within
twenty seconds, and the console has a file nobody has chosen. A game that ran for
a while and then fell over did not fail to *start*, and a console with everything
its cores read already on the disk has nothing for anybody to go and find.
Everything else — a bad dump, a core that fell over, an emulator that could not
open the display — stays in the log, which is where it belongs. A panel on every
failed launch would be a shell interrupting people about things it does not
understand.

Where the files go is libretro's declaration and not a guess. A core that names a
folder (`pcsx2/bios`) is given every *dump* in the one you chose, because the
emulator reads all of them and lets you pick a region in its own menu. A core
that names a file (`dc/dc_boot.bin`) is given that file, matched without regard
to case. Nothing is renamed or fetched; a file you already had moves from one of
your folders to another.

**A folder with no BIOS in it is refused, and asked again.** This is the one
press in the integration that has to be able to go round twice: a boot ROM is
one file among thousands, you are looking for it in a chooser on a television
across the room, and the first guess is very often the wrong folder — or the
right folder with the dump still inside the archive it came down in. So the
panel afterwards says *No BIOS in that folder*, names what was looked for in
libretro's own words, and offers **Choose another** beside **Cancel**. The
console the question is about, and the game whose press asked it, are both still
standing behind that panel: getting it right on the second try still ends with
that game starting.

Nothing is written down that says the console is set up, either. What decides
that is what is on the disk, read again the moment the copy finishes — so a
folder that answered nothing leaves the row saying *Not added*, leaves the
warning under every game on that console, and raises the same question on the
next press.

Where a declaration names a file, being refused means no file of that name was
in the folder. Where it names a *whole folder* there is no name to match on, and
this is the one place a chooser could empty a downloads directory into
RetroArch — so what goes in is what could be a boot ROM. That is a test for what
a dump is **not**: an archive, a document, a picture, a recording, a program, or
text of any kind. A dump itself is an opaque blob and there is nothing positive
to look for that would not be a table of consoles, which this integration does
not have anywhere. Files a core wrote beside its own BIOS — pcsx2's four-byte
`.mec`, its `.nvm` — are blobs too and go in with it; it costs nothing, and what
an emulator wants beside its firmware is not this shell's business.

It matters more than it sounds. Before it, pointing the chooser at a folder of
photographs copied the photographs in, and a firmware folder with *something* in
it read as answered: the row said *Added*, the warning came off every game on
that console, and the emulator started to a black screen with nothing anywhere
saying why.

The same question is a row on that emulator's own settings page — **Settings →
Games → RetroArch → LRPS2 → PlayStation 2 BIOS** — and it is there whether or
not a BIOS has been chosen. **Every core that reads one has that row**, including
the cores libretro says can manage without: melonDS calls all eight of its files
optional, and somebody who owns a Nintendo DS and dumped its firmware wants those
files used. Optional means "no warning", not "nowhere to put it". Choosing one is a thing people get wrong: the wrong
region, a bad copy, the other console's. A row that vanished the moment it was
answered could only be reached again by taking the file back off the disk.

It is on the *emulator's* page and not beside somebody's consoles, because that
is what it belongs to: pcsx2 reads a PlayStation 2 BIOS and the page next to it
does not. Nobody has to go looking for it — pressing a game that cannot start
raises the panel, and the panel walks the cursor there.

libretro's firmware list is not only boot ROMs. pcsx2 declares
`pcsx2/resources/GameIndex.yaml` as required too, and that one is fetched with
the core; anything this integration downloads for itself is left off the row,
because asking somebody to find a file they have never had is worse than not
asking.

Two guards, against a folder chosen by accident rather than against any real
collection: nothing over 64 MB is treated as a BIOS, and at most 64 files are
taken. A PlayStation 2 BIOS is about four megabytes.

**The game starts when the BIOS lands**, where the question came from pressing
one. The walk to answer it moves the cursor off that game, so the cursor is
carried back to the row before it starts — a press that ends four columns away
from what was pressed is a press nobody can follow. Reached from Settings
instead, there is no game to go on to and a panel says what was copied.

Which console the chooser is answering for is read off the row it opens *from*,
every time, and not only from the panel over a game. The row under an emulator's
settings page is reached with no game in hand at all, and a console left over
from a panel dismissed an hour ago used to send a Nintendo DS dump looking for
PlayStation 2 file names — and, where the folder held one, start the PlayStation
2 game from an hour ago.

The panel's walk goes to that console's row and to no other. It also **waits for
the page it lands on**. An emulator's settings
page is built out of what its core answered, and the cores are asked when
somebody arrives in Settings rather than at every login; this walk is the one
route that arrives there without anybody walking. So it asks, waits for the core
that owns the row to answer — they answer one at a time, and it is not the
first — and gives up after eight seconds rather than moving the bar under your
hands minutes later. Before that, *Choose folder* did nothing at all on a fresh
session — and, once it did something, a walk made a moment too early found
melonDS's row and answered a PlayStation 2 question with a Nintendo DS folder.

### What counts as having the BIOS

Not every file a core lists. duckstation names all three PlayStation BIOS
regions, o2em four Videopac models, uae4arm six Kickstarts — nobody owns the set
and nobody has to, because one of them is what your own games run on. A row that
counted the other two as missing would go on saying *Not added* on a console that
was fully set up, and go on offering a chooser no folder on earth could answer.

So which of them stand in for each other is read out of where they live: what a
core wants in one folder is one job, and having any of it is having what that job
needs. duckstation's three sit side by side and are three spellings of one. It is
a reading of libretro's own layout rather than a table of consoles, which is the
rule this whole integration is held to.

Nothing here refuses a press. It decides what a settings row says, and what the
panel over a game that has already failed offers to go and look for.

Two things can still leave a console unplayable, and both are said rather than
worked around:

* **Every core for it needs firmware and you have not got one.** The press starts
  the emulator, the emulator comes straight back, and the panel over it asks
  where a BIOS is — see **A missing BIOS** above.
* **A core downloads and will not load.** libretro builds its cores against a
  general-purpose Linux and a flatpak RetroArch runs against a runtime that is
  not one, so a core can arrive whole and fail in the dynamic linker. The
  install checks with `ldd` *in the environment the core will be loaded in*.

  A core that will not open is taken back off the disk and its name written to `$XDG_CACHE_HOME/lxb/cores-that-will-not-load`, so the
  next press does not spend another download on it. Delete that file to try
  again after a RetroArch or core update.

## An emulator's own settings

Each installed core gets a page under **Settings → Games → RetroArch**, and
nothing on it is written down in this repository: the core declares what it can
be set to and the helper asks it, so the page is whatever that emulator's
authors put in it, in their order and under their names.

**The question is asked when somebody reaches Settings**, not at start-up. It is
not a cheap one: every installed core is loaded into a process to be asked, and a
machine with a dozen of them would map a dozen emulators at every login for a
screen most people open twice — once when they install an emulator, once when a
game looks wrong.

Reaching Settings is a long way from a core's own page: down the column to Games,
in, down to RetroArch, in, and down again. The answer has those presses and their
animations to arrive in, and it arrives the way a fetched core's answer already
did — the column is built again and the rows are simply there.

**There is a short way.** The menu over a console — and over any one of its games
— offers **Emulator settings**, which carries the cursor straight to that
emulator's page. It is offered whenever something on this machine plays the
console, and not only once the page exists: on a session where nobody has been to
Settings there are no pages at all, so a row that waited for one would be greyed
on every fresh login. The press asks for them and waits up to eight seconds for
the one it wants, then gives up rather than moving the bar under somebody's hands
a minute later. An emulator that declared nothing and needs no BIOS has a row
instead of a page; the walk stops on it and lets it say where its settings really
are.

The answer goes stale when a RetroArch is found and again whenever a core
finishes installing, and it is asked once per staleness however long somebody
stands in Settings. That second staleness matters: a core fetched during a
session was not on the disk when the shell first asked, and without it the new
emulator's page did not exist until the shell was started again.

**Not every emulator answers when it is merely asked.** Most hand their table
over during `retro_set_environment`, which is the first call a frontend makes
and costs nothing. Some declare nothing there at all: on this machine LRPS2
makes three calls and none of them is a table, and dolphin makes none.

So the helper asks again, further in, and it is a ladder rather than a single
deeper ask because each rung is one more emulator entry point that can fail:

1. `retro_set_environment` — melonDS, Mesen and PPSSPP answer here.
2. `retro_init` — LRPS2 answers here, with sixty-six settings in five groups.
3. `retro_load_game` **with a null game** — dolphin answers here, with
   ninety-nine in twelve.

Nobody's game is ever opened. A null `retro_game_info` is what `libretro.h`
defines for a frontend starting a core with no content, and it is the one
argument that gets an emulator to declare without playing anything. It needs no
BIOS and no disc: LRPS2 answers with neither.

Three things make this survivable.

**A process of its own.** Rungs two and three are calls into an emulator, not
questions put to a shared object, so what a crash costs is one core's settings
page rather than the answers every core before it gave.

**The answer leaves before the crash.** dolphin dies *every time* — a moment
after handing over its table, carrying on into video setup where a frontend this
small has nothing for it. So the record is written from inside the core's own
callback, the instant a table arrives, rather than after the entry point
returns. There is no return to wait for. The caller keeps the newest line it
saw, which is why reading what has been declared must not empty it: a core may
declare in two calls, and the second line has to carry both.

**The calls a core makes on the way are answered.** Refusing them is what stops
an emulator getting far enough to say anything, and one of them is fatal rather
than merely unhelpful: the libretro pattern for `GET_LOG_INTERFACE` is to take
the frontend's logger *or* fall back to its own, and a core that forgets the
second half keeps a null pointer and calls it. LRPS2 segfaults inside
`retro_init` for exactly that reason, which read for a long time as an emulator
that could not be asked at all. It is also why `retro_init` was once believed to
crash PPSSPP; it does not.

An emulator taken this far writes things down on its way past — dolphin lays out
a whole `User` tree — so the save directory it is given is a scratch folder and
never RetroArch's own. The system folder is the real one, because the point of
asking is to hear the truth about this machine.

The whole run costs about a fifth of a second for five cores, two of which are
taken all the way. It happens when somebody reaches Settings, once per
staleness, rather than at startup.

**A core that answers none of the three rungs still gets a row**, with the
information mark and a line saying its settings are in RetroArch's own menu with
a game running. Left off the page, an emulator somebody had just installed was
simply missing, which reads as the install having failed. The line does not
promise that playing something will bring the settings here — for a core that
only speaks with a real game in it, nothing the shell does afterwards will hear
it.

**Two names per setting.** A V2 table carries a full name and a short one for
when the setting is already standing under its own group. LRPS2 calls one of
them `Emulation > EE Cycle Rate` and the other `EE Cycle Rate`; the short one is
used where there is a group above it and the full one where there is not.

## Cores that ask for an executable stack

A libretro core can arrive whole, name every library it needs, and still be
impossible to load. A shared object carries a `PT_GNU_STACK` program header whose
flags say what kind of stack it wants, and a few are built asking for an
executable one — nearly always by accident, from an assembly file missing its
`.note.GNU-stack` marker. A current glibc refuses outright:

```text
cannot enable executable stack as shared object requires: Invalid argument
```

libretro's own build server ships melonDS this way. Nothing this integration
checked caught it: the download is a valid ELF of the right size, and `ldd` finds
every library it names, because the failure is in the loader rather than in what
the core links against. What somebody saw was a Nintendo DS shelf that scanned,
listed its games, offered to play one, and did nothing at all when asked —
and RetroArch on its own failed the same way, so there was nowhere to go and find
out why. It took the core's settings page with it by the same door, since the
options probe reaches a core through `dlopen` too.

So the integration takes the bit off. It is one bit in one program header: the
file is not moved, relinked, or rewritten anywhere else, and a file that is not an
ELF — or is of a shape the reader does not recognise — is not opened for writing
at all. Clearing it cannot break a working core, because an object that genuinely
executed its stack is one that was already failing to load.

It runs in three places, all of them cheap enough to run every time: on the
directory during a scan, on the same directory before the options probe, and on a
core the moment it has been downloaded — that last one before the check that
would otherwise throw the core away and refuse to fetch it again.

Only the user's own core directory is repaired. A core under `/usr/lib/libretro`
belongs to the distribution's package manager, which would put it back on the
next update, and writing there needs a root this integration never asks for.

## The console marks

Not a setting; there is nothing to configure. Each console the integration knows
has a drawing of its own, and they ship in the `lxb-retroarch` package under
`share/lxb/glyphs/console-*.svg` — read out of the data directory at startup,
which is how a package brings its own marks to a shell built without them.

Each is the machine drawn flat on, in as few parts as it can be recognised from,
and nothing in the file is shaded: the shell measures the outline into a distance
field and its own shader makes the water. A folder whose name the table does not
know has no machine behind it and falls back to RetroArch's own mark, as does a
console whose drawing did not ship.

**They are drawn to the hardware's own measurements**, and that is the rule the
set is held to rather than a preference. A Game Boy Advance is 144.5 by 82 mm
with a screen half again as wide as it is tall; a DS has two 256-by-192 panels
and a 3DS has a 400-by-240 over a 320-by-240; a PSP has four face buttons in a
diamond and a Game Gear has two side by side. Every one of those is a thing
somebody checks without meaning to, and getting it wrong is the difference
between a drawing of a console and a drawing of *the* console.
`art::drawings::every_screen_is_the_shape_its_panel_was` holds the screens to
it: the panel figures live beside the drawings, and a redraw that disagrees with
the hardware fails rather than shipping.

Where a measurement has to be given up it is given up for the material and said
so in the file. The bevel this shader rolls over every edge is about two and a
half units of a thirty-two-unit cell, so a wall thinner than that has no flat
face and comes out melted — which is why the PSP's screen covers under half its
width here and over half of one in life: the buttons on either side of it need a
wall to sit in.

Adding one means a drawing under `crates/lxb-retroarch/glyphs/`, named
`console-<key>.svg`, and the matching `glyph:` on that console in
`crates/lxb-retroarch/src/consoles.rs`. The two are checked against each other:
a name with no file is a column wearing the fallback with nothing in the log to
say why, and a file with no name in front of it is a cell of the shell's atlas
spent on a drawing nothing asks for.

## The games' artwork

Not a setting either, and there is no key for it. Every game in the folder gets
the cover on its row and the picture behind the display from
[libretro's thumbnail collection](https://thumbnails.libretro.com) — the box art
and one screenshot, the same two RetroArch fetches. The screenshot is blurred on
its way to the display; a console's screen is three hundred pixels tall, and
enlarged honestly across a television it is a wall of squares.

**Each shelf is drawn at the shape of its own console's boxes.** A Nintendo DS
case is wider than it is tall, a Wii case is taller than a PlayStation 2 one and
a UMD case is taller again, so one card shape for all of them leaves a cover
floating in a button it cannot fill. The shape is measured off the covers
themselves — the first few a console has, the middle of what they measure, taken
to a hundredth so that two scans differing by a pixel do not move the column —
rather than looked up in a table of consoles, which would be this shell
asserting the dimensions of artwork it did not make. Every card on a shelf is
one shape, including the games with no cover, and a console with nothing to
measure yet keeps the shape it had before. What is held constant between shelves
is how much of the glass a row takes up, so a squarer cover is drawn wider and
shorter and every row of every console still weighs the same.

**A game is not asked for by its file name.** libretro's names are the names of
dumps — `Tekken 6 (USA) (En,Fr,De,Es,It,Ru)` — and asking for `Tekken 6.iso`
answers 404, which is why RetroArch shows a hand-sorted collection no artwork
either. Instead the listing of each console's shelf is fetched once and every
game is matched against it with the tags off both sides, the punctuation
dropped, and `Legend of Zelda, The` put back the way the box says it. Where
several dumps reduce to one game, a beta or a demo loses to the release, then
region decides, then the plainer name; the same shelf answers the same way every
time. Nothing clever is done with numbers — `Mega Man X` must not become
`Mega Man 10` — so a game whose name is too far from the database's simply gets
none, and keeps the mark its row always wore.

Everything lands under:

```text
$XDG_CACHE_HOME/lxb/retroarch-art/
  .shelves/<System Name>.list                    the names, one per line
  <System Name>/Named_Boxarts/<Game Name>.png
  <System Name>/Named_Snaps/<Game Name>.png
  <System Name>/Named_Snaps/<Game Name>.none     the server has none
```

libretro's own layout, in this shell's cache and never in RetroArch's thumbnail
folder: what somebody sees in the emulator's own interface is the emulator's
business. The empty `.none` file is what stops a game with a cover and no
screenshot asking for that screenshot once a session for the rest of the
machine's life; a listing is believed for a fortnight before it is fetched
again. Deleting the whole directory loses nothing but the download.

**It is not small.** The two pictures come down at the size libretro publishes
them — measured across a mixed handful of consoles, about half a megabyte a
game, so a five-hundred-game collection is a couple of hundred megabytes, plus
a few hundred kilobytes for each console's listing. That is a cache, in the
cache directory, and it can be deleted at any time; what it buys is that a
column of games looks like a shelf of games.

The shell asks for whatever is missing **once per folder per session**, quietly:
no panel, and the covers appearing down the column are the feedback. Three rows
ask for more. **Settings > Games > RetroArch > Get the artwork again** looks for
every game afresh, ignoring what is on this disk — for a collection that has
grown, or a game that has since been renamed. The menu over a **console** offers
the same for that shelf alone. And the menu over one game offers **Get the
artwork** for that one, which is what to press after renaming it; if libretro
still has nothing, a panel says so rather than the row quietly staying as it
was.

## Pictures of your own

libretro has a cover for nearly everything anybody owns, and two cases where it
has none: a game whose file name is too far from the database's for any match,
and a game whose published cover is not the edition you have. The menu over a
game answers both — **Choose a cover** and **Choose a background** open a walk
through your own files, one folder at a time, exactly as the wallpaper is
chosen. The row it hangs off is the game's own, and the films are left out:
neither of a game's pictures can be one.

**A background is chosen by seeing it.** Standing on a picture puts it across
the whole display, the way the wallpaper picker works, because that is what a
background is going to be. A cover is not: it is a card an inch high, so that
walk shows each picture on its own row instead and leaves the display alone —
a full-screen preview of something that is never going to happen would be worse
than none.

A background of your own is also drawn **as it is**. libretro's screenshot is a
photograph of a console's screen, three hundred pixels tall, and is blurred on
its way across a television because enlarged honestly it is a wall of squares.
Yours is at whatever size you chose it, and goes to the display the way a
wallpaper does.

What you choose is **copied**, not pointed at, so tidying the folder you found it
in does not take the cover with it:

```text
$XDG_DATA_HOME/lxb/game-art/
  <console>/<the game's own file name>.cover.png
  <console>/<the game's own file name>.background.jpg
```

Named after the game's file so the directory can be read, and one directory per
console because the same dump can sit on two shelves. A picture of your own
stands where libretro's would and **stays there through every fetch afterwards**
— a run that put the published cover back over yours would be the shell
overruling a choice you made.

Once one is chosen the menu row that chose it becomes **Remove your cover** — or
your background — in the same place on the same list, so the menu does not
change shape under your hand. Removing it puts the row back to
libretro's, or to the console's mark where libretro has nothing. Deleting the
file by hand does the same thing: the copy *is* the record.

The shelf takes its shape from these too, which matters most for a console
libretro has never published artwork for: every row of it wears a picture you
put there by hand, and the cards are cut to their shape rather than to the one a
column of marks falls back to.

## `picture-in-picture`, in `shell.toml`

`~/.config/lxb/shell.toml`, top level, beside the keys above:

```toml
picture-in-picture = true
picture-in-picture-size = "medium"
picture-in-picture-place = "top-right"
```

What happens to the small window a browser puts a video into when the user asks
for picture-in-picture. Written from Settings > System > Picture-in-Picture,
which is a switch, a size and a corner.

Such a window is found by its **title**, which is `Picture-in-Picture` and is
what every browser that has the feature calls that window. It cannot be found
any other way: the window belongs to the browser and calls itself by the
browser's name, which is also what the window the video came out of calls
itself.

A window that answers to it is taken out of the layout every other window is
under. It is not maximized, it is not given the keyboard, it is not listed in
the guide as something to switch to, and it is drawn **in front of everything
the session has** — over a fullscreen game, over the start screen, and over the
guide, which is the one surface nothing else is allowed in front of. It is still
clicked on, exactly where it is drawn — and looked for in front of everything
else, as it is drawn — which is how its own play button is pressed. A press on
it never takes the keyboard: whatever the user was working in goes on hearing
every key. And the application it belongs to is never put to sleep while it is
on screen, however completely the rest of that application is covered.

| Key | Values | Default | Meaning |
| --- | ------ | ------- | ------- |
| `picture-in-picture` | bool | `true` | Float such a window at all. |
| `picture-in-picture-size` | `small`, `medium`, `large` | `medium` | A sixth, a quarter or a third of the display's width. |
| `picture-in-picture-place` | `top-left`, `top-right`, `bottom-left`, `bottom-right` | `top-right` | Which corner it sits in. |

How tall the window is at that width is the window's own business: the
compositor asks it what shape it wants to be — a configure carrying no size,
which is xdg-shell for *choose one* — and follows the answer, so a
four-to-three video is not letterboxed into a widescreen box. Sixteen to nine
stands in until it answers, and the answer is the first size it draws that it
was not told to draw: a client's very first commit is often a one-pixel
placeholder, which is not a window and is not read as one. It goes on being
listened for, so a video swapped for one of another shape in the same window
takes that shape too. A second picture-in-picture window opened while the first
is still up stands below it in a column from the same corner, in the order they
started floating.

A mouse can move and resize such a window, which is the one thing the three keys
above do not describe. Eight logical pixels in from each edge of the surround is
a band that resizes it, where two bands meet is a corner that resizes it both
ways, and dragging anywhere else carries it; the pointer takes a resize shape
over the edges. A press in the middle goes to the client at once and only becomes
a drag once the hand has travelled four pixels, at which point the client is told
the pointer left — so a play button still answers a click, and a click that turns
into a drag does not press it. The opening keeps the client's shape throughout,
so every handle scales the window rather than restretching the video, and it can
be made no smaller than the smallest the layout draws, no larger than the screen,
and not dragged off it or onto another one.

The right button on it raises the shell's context menu — Move, Resize, Move to
next display, Move to previous display, Close, Cancel. Move and Resize hand the
window to the pointer until the next click, from the middle of it and from the
corner with the most room to grow into; a click of any other button puts it
back. The two display rows are the same request the guide's window menu sends,
and neither wraps: a row that cannot be taken is disabled and still drawn. Close
asks the window to close, which is what puts the video back in the page it came
from — not the Close that ends an application, because the application here is a
browser.

A context menu is drawn **in front of** the floating windows instead of behind
them, and a press on its panel goes to the shell — otherwise a window resized to
fill the screen could never be made small again. It is given a surface of its
own for this, a child of the display's with nothing else on it, rather than
lifting a rectangle of the shell's main surface: a panel is rounded and a
rectangle is not, and the bounding box laid four square corners of the start
screen over the video. On its own surface the panel is exactly its own shape, and
its input region is what decides whose a press is, so the shape the pointer finds
is the shape the eye finds.

That surface is in front of every window on the display, not only the floating
ones, so a menu of the bar's does not outlive the bar stepping behind an
application. The moment the compositor says an application is in front of the
driven display and the bar is not standing over it on purpose — by the guide's
own row, or as the guide's menu — a context menu still up is put away, whatever
raised it and however the application got there: a launch handing the display
over, a window raised on its own, Valve's client given sight of one of its own.
It could not be driven from there anyway, since the pad's buttons belong to the
application once one is in front. Only the two menus that are meant to be over
an application stay: the floating window's own, and the one raised over an
application's file question.

The panel's glass refracts the video, the way it refracts everything else it
stands on. The shell cannot see a frame of somebody's video, so the compositor
draws it a small picture of what is behind that surface and it refracts that —
absorbed on the way, because the shell's own ground is deliberately dark and a
pane that let a bright film through at full strength would stop being readable.

A controller reaches such a window **while the guide is open**, which is the one
moment the user is plainly not using the application underneath. The right stick
pressed hands the guide's directions to the videos over it and pressed again
hands them back, as Back does; the selected one is marked by the compositor in
the shell's accent, frame and glow, breathing. The right stick then carries it,
exactly as a mouse held on it does. The D-pad and the left stick walk between the
videos on that screen by where they are on it, and the top face button raises the
same menu the right button raises — where Move and Resize become right-stick
drags ended by Accept and undone by Back, since there is no pointer to hand the
window to. Only that screen's videos: the guide is only ever on the screen being
driven, and such a window stays on the screen it opened on.

A window that has been moved or resized **leaves the column**: it keeps where it
was put, and the place it stood in is free for the next window that starts
floating. `picture-in-picture-place` and `picture-in-picture-size` stop applying
to it until either is *changed*, which puts every floating window back into the
column — a press on the Settings page is the user saying where they want their
videos.

`picture-in-picture = false` treats such a window as the application window it
otherwise is — maximized, listed, focusable — which is what this session did
before it could be asked. A size or a corner spelled some other way is ignored
with a line in the log, and that half keeps the answer it had.

The window is drawn with a hairline rounded surround — three logical pixels,
fixed, with its corners rounded at eight — and a shadow under it. The two
numbers are one decision: the client's square corner sits √2·(r − b) from the
centre of the outer arc and is hidden only where √2·(r − b) ≤ r, so a surround
this thin can only round a corner about that far. What the shell's menu radius
still sets is how far the window is held off the edges of the screen. The
surround is not a decoration:
a client's buffer is a rectangle with four square corners, and the surround is
what covers them — see `crates/lxb-protocol/src/pip.rs`, where the arithmetic
that makes it hold is written down and shared with the compositor that paints
it. Nothing is drawn outside that shape: a client with its own drop shadow —
Firefox's picture-in-picture window has one — or one drawing larger than the
size it was given is scaled down to fit the opening and clipped to it. And
nothing shows through it either. The surround is painted half a pixel *over* the
picture, because the opening is a fractional rectangle and everything drawn is
whole — butt them together and the half pixel left over is a one-pixel line of
the session down one side of the video — and behind the window the opening is
filled with the surround's own colour, so a client that does not cover it — one
still starting up, or one that will not take the size it was given — is centred
and letterboxed on more surround rather than leaving a hole through to whatever
is behind.

It is carried out by the compositor, which is what places windows. Nothing is
remembered there: the shell says what this is as soon as it connects, which is
long before any browser exists to put a video in.

## What the displays were last set to

`$XDG_STATE_HOME/lxb/displays.toml`, or `~/.local/state/lxb/displays.toml`.

Not a file to edit. The compositor writes it whenever the shell changes a
display, and reads it back before it lights anything.

It exists because of when the shell can speak. Every display setting arrives
over `lxb_shell_v1`, and the shell cannot send one until it has a Wayland
connection — about a second after the compositor has already brought the
displays up in whatever it had. So the compositor would light a display, and
then a second later be told to drive it in HDR instead. Turning HDR on or off
re-drives the pipe and the panel re-locks behind it: measured on an RX 9060 XT,
**169 ms** of dark for coming up in the wrong one, and **196 ms** more for being
corrected. Two black screens, either side of a second of un-warmed picture, on
every single login.

Remembering means the first commit already carries the right mode, orientation
and colour pipeline, so the driver finds nothing to change and cancels the
modeset. It is what makes a hand-over seamless rather than merely short, and it
is why `LXB_HOLD_DISPLAY` alone was not enough.

The file uses the same `[[output]]` shape as `config.toml`, and **`config.toml`
wins**, key by key. A key written by hand is a decision and is never overridden;
remembered state only fills in what the config leaves unsaid. So a machine that
pins `hdr = false` stays in SDR however often the shell is asked for HDR, while
one that pins only a resolution still comes up in the colour it was left in.

Deleting it costs one blinking login while it is written again. A file this
compositor cannot parse is reported and ignored, never repaired: it is more
likely to be a newer version's than a corrupt one.

## `[input]`

| Key                   | Type    | Default | Meaning |
| --------------------- | ------- | ------- | ------- |
| `keyboard_layout`     | string  | `"us"`  | xkb layout. |
| `keyboard_variant`    | string  | `""`    | xkb variant. |
| `keyboard_options`    | string  | unset   | xkb options, e.g. `"ctrl:nocaps"`. |
| `keyboard_model`      | string  | `""`    | xkb model. |
| `keyboard_rules`      | string  | `""`    | xkb rules. |
| `repeat_rate`         | integer | `25`    | Key repeats per second. |
| `repeat_delay`        | integer | `600`   | Milliseconds before repeat starts. |
| `tap_to_click`        | boolean | `true`  | Touchpad tap-to-click. |
| `natural_scroll`      | boolean | `false` | Reverse scroll direction. |
| `disable_while_typing`| boolean | `true`  | Ignore the touchpad while typing. |
| `pointer_accel`       | float   | `0.0`   | libinput acceleration, `-1.0..=1.0`. |
| `scroll_speed`        | float   | `1.0`   | Multiplies the scroll distance libinput reports. Must be greater than zero; use `natural_scroll` to reverse it. |

`keyboard_layout` and `keyboard_variant` are what the session **starts** at. The
shell can change them while it runs — Settings > Input > Keyboard > Keyboard
layout, over `lxb_shell_v1.set_keyboard_layout` — and once somebody has picked a
row there, the shell's own `keyboard-layout` in `shell.toml` is what a later
session comes up with. Until then these two are in force, so setting a layout
here by hand still works and is not overwritten. Nothing is written back to this
file.

Whatever is in force is exported to the session's children as
`XKB_DEFAULT_LAYOUT` and `XKB_DEFAULT_VARIANT`, so a program that draws its own
picture of a keyboard — the login screen's on-screen board, a toolkit
application's search keyboard — prints the right letters on it. Like the cursor
size beside it, a process reads these when it starts, so this reaches the next
client and not one already running; every Wayland client is sent the seat's real
keymap either way.

Both of those programs also read `keyboard-layout` out of `shell.toml`
themselves, and read it first. Under this session that is the same answer; away
from it, it is the better one. A variable is copied into a program as it starts,
so one left running across a change to the setting holds the answer from before
it — and the login screen is not in this session at all. It takes the layout of
whichever account is being looked at, out of that account's own settings, and
falls back to this machine's keyboard for an account that has never chosen one.

A layout that xkbcommon cannot compile is refused and the keyboard is left
exactly as it was, rather than falling back to something else: a machine whose
keyboard silently became American is a machine somebody may not be able to type
their password into. The refusal is logged, and the shell is told what is really
in force so its page cannot go on marking a row that did nothing.

## `[[output]]`

Repeat this table once per display. Entries are matched by connector name
(`DP-1`, `HDMI-A-1`, `eDP-1`, …). The name `"*"` matches any output that has
no more specific entry; an exact match always wins.

Connector names are printed at startup, and `wayland-info` will list them
from inside a running compositor.

| Key             | Type              | Meaning |
| --------------- | ----------------- | ------- |
| `name`          | string            | Connector name, or `"*"`. |
| `mode`          | string            | `"2560x1440@144"`, `"1920x1080"`, or `"preferred"`. |
| `position`      | `[x, y]`          | Pins the output. Without it, `output_layout` decides. |
| `scale`         | float             | Fractional scale, e.g. `1.5`. |
| `transform`     | string            | `normal`, `90`, `180`, `270`, `flipped`, `flipped-90`, `flipped-180`, `flipped-270`. |
| `enabled`       | boolean           | `false` leaves the display unlit and out of the layout. |
| `adaptive_sync` | boolean           | Parsed, but not yet applied. |
| `hdr`           | boolean           | Drive this display in high dynamic range. Default `false`. |
| `hdr_sdr_brightness` | integer      | Luminance plain white is sent at, in cd/m². Default `200`. |
| `hdr_srgb_intensity` | integer      | How far sRGB colour is stretched towards BT.2020, `0`–`100`. Default `0`. |
| `hdr_peak_brightness` | integer     | Peak declared to the display, in cd/m². Omit to use the display's own. |
| `night_light`   | boolean           | Warm this display's picture. Default `false`. |
| `night_light_temperature` | integer | How warm, in kelvin — lower is warmer. `1000`–`6500`, default `4000`. |

Refresh rates are matched to the closest mode the hardware reports, so `@60`
will select a 59.94 Hz mode. If the requested resolution does not exist at
all, the connector's preferred mode is used and a warning is logged.

`mode` is what a display **comes up in**. The shell changes it at runtime from
Settings → Display → Resolution and → Refresh rate, and writes what it chose
per connector in its own file (`~/.config/lxb/shell.toml`), in this same
`WIDTHxHEIGHT@REFRESH` format. On a machine that boots into `lxb --shell`
the mode is therefore normally decided there; set it here for a session with no
shell, or to choose what a fresh profile starts at. A size the connector does
not list is refused whichever file asked for it, and the display keeps the mode
it has.

`transform` is the same arrangement for which way up the picture is drawn. The
shell changes it from Settings → Display → Orientation and writes it back under
this same key and these same spellings, so a line can be moved between the two
files and mean the same thing. The page offers the four rotations; the four
`flipped-*` values are here only, and a display set to one of them is named on
that page with none of its four rows marked.

Turning is the compositor's own drawing rather than anything the connector
does: the picture is composited turned and scanned out at the mode's own
pixels, so it needs no hardware support and works on any display this
compositor owns. What it changes is the display's *logical* size — a quarter
turn swaps its width and height, which moves the outputs laid out after it —
so `position`, `scale` and the window layout are all resolved against the
turned size.

`position` is also what takes a display out of the shell's reach. Displays
without one are laid out in the order they were plugged in, and Settings →
Display → Display order changes that order: choosing a place for a screen
trades it with the screen standing there, and the shell writes the whole
resulting arrangement under a per-connector `order` key in its own file
(`~/.config/lxb/shell.toml`, counted from one). A display pinned here is where
this file says and takes no part in it, so it is left off that page rather than
listed with a setting that would do nothing — as is every display when
`output_layout` is `mirror`, where the screens share one region and there is no
first one to be.

The arrangement is the one display setting the compositor does not remember
across a session. The rest are written to `displays.toml` because coming up in
the wrong one costs a modeset, which is a black screen; an arrangement costs a
relayout, and at login there is nothing laid out yet. So the shell keeps it and
sends it when it connects.

### High dynamic range

The four `hdr_*` keys are what the session **comes up in**. The shell writes
the same settings at runtime from Settings → Display → HDR → *the screen*, and
what it writes is remembered per connector in its own file
(`~/.config/lxb/shell.toml`), so a machine that boots into
`lxb --shell` is normally configured from there rather than from here. Set
them here for a session with no shell, or to choose what a fresh profile starts
at.

Turning HDR on does two things at once. The connector is told the signal is
BT.2020 with the ST 2084 (PQ) transfer function, through the `Colorspace` and
`HDR_OUTPUT_METADATA` properties; and the CRTC's colour pipeline
(`DEGAMMA_LUT` → `CTM` → `GAMMA_LUT`) is loaded to re-encode the composited
sRGB picture into that signal. Both are needed: a display told to read sRGB
numbers as PQ shows a scene that is far too dark and the wrong colour.

All five properties are set in **one** atomic commit, and that commit is first
offered to the driver as a `TEST_ONLY` request. A pipeline the hardware will
not take is therefore rejected by a commit that changed nothing, rather than
attempted on a live display; the log says so and the display stays in SDR.
Leaving the session puts every display back into SDR before giving up DRM
master, so quitting always returns a readable console.

HDR is refused, and logged, unless the driver is on the atomic interface and
the display's EDID advertises ST 2084 *and* the driver publishes
`HDR_OUTPUT_METADATA` and at least a `GAMMA_LUT`.

`hdr_srgb_intensity` needs a `DEGAMMA_LUT` as well, because it is implemented
as the `CTM` and a matrix is only a gamut conversion when it acts on linear
light. Without one the sRGB decode is folded into the gamma curve instead —
brightness stays correct — but the matrix is set to identity, which pins the
gamut at the vivid end whatever this key says: sRGB's primaries go out as
BT.2020's. Not all hardware exposes a degamma stage; amdgpu on RDNA 4, for
instance, does not on its display pipes. `lxb` logs which of these
happened when it drives a display into HDR — look for `pipeline=` on the
`driving this display in HDR` line — and tells the shell, which stops offering
the setting on displays that cannot honour it.

To see what a connector publishes before turning anything on, run with
`RUST_LOG=debug` and look for the `colour pipeline` line logged when each
display is connected. It reports whether the driver is atomic, which of the
five properties exist, and the entry counts of the two LUTs.

`hdr_sdr_brightness` is the setting that matters most, because nothing LineXinBar
composites is HDR content: every application and the shell itself are SDR, so
this alone decides whether the session comes out dim or blinding. sRGB's own
reference is 80 cd/m², which describes a darkened grading suite.

`hdr_srgb_intensity` decides what happens to colour inside BT.2020's much wider
gamut. At `0` sRGB's colours are placed where they actually belong, so the
session looks exactly as it did in SDR. At `100` the numbers are passed through
untouched, so sRGB's red is displayed as BT.2020's red and everything comes out
far more saturated — the "vivid" mode a television ships in. In between is a
blend of the two.

### Night light

The two `night_light_*` keys are the blue light filter, and they are what the
session **comes up in** for the reason the `hdr_*` keys are. The shell writes
the same setting at runtime from Settings → Display → Night light → *the
screen*, per connector in its own file (`~/.config/lxb/shell.toml`), so a
machine booting into `lxb --shell` is normally configured from there.

It is the CRTC's `GAMMA_LUT`: green and blue scaled down against red, so the
picture goes warm without anything getting brighter. That means it needs the
atomic interface and a gamma ramp and *nothing else* — no EDID claim, no
infoframe, nothing of what the link can carry — so unlike HDR it works on
essentially any display this compositor owns, including SDR laptop panels. A
nested session is the exception: it owns no CRTC, so there is no ramp to load.

The filter composes with HDR rather than competing with it. Both are encoded
into the same ramp and committed together, so turning HDR on does not undo a
warm picture and turning the night light on does not undo the PQ encoding. On
an HDR display the white point is applied in linear light, in front of the PQ
encode; on an SDR one the ramp decodes, scales and re-encodes sRGB. They are
the same white point expressed for two stages, so a display does not change
colour when it changes signal.

`6500` is ordinary daylight and is exactly the picture with the filter off: the
curve is normalised so that temperature is the identity to the last code, not
merely close to it. Below roughly 1900 K a black body has no blue in it at all
and the ramp takes that channel to zero, which is why the shell's own page
stops at 2000 K; a value set here is still honoured, and anything outside
`1000`–`6500` is brought to the nearest end rather than refused.

**There is no schedule here, deliberately.** Keeping hours means a clock and a
time zone, and the compositor owns neither — the shell works these out against
the machine's local time and sends only whether the light should be burning
now. That is also where the sunset-to-sunrise schedule lives, and where the
coordinates it needs are read: the shell's own file, not this one. So these keys
are an always-on setting: for a session with no shell, or for what a display is
warmed to until one connects.

### How large applications draw themselves

**There is no key here for it, deliberately**, although it is the compositor
that carries it out. Settings → System → Application scaling gives every
application a logical window some fraction smaller than the display and tells it
— over `wp_fractional_scale_v1` — to fill that window with the display's own
pixels, so an interface comes out larger without losing a pixel of sharpness.
The shell keeps the number, in its own file (`~/.config/lxb/shell.toml`), and
sends it over `lxb_shell_v1` as soon as it connects.

That is the whole difference from the two settings above. Those are here because
the compositor lights the displays a second before the shell can speak, and
being corrected afterwards costs a black screen. Nothing is on screen to
correct here: every application on the session is started *by* the shell, always
after it has said what this is, so no window is ever configured at the wrong
size in the first place.

The shell's own surfaces are not affected — it is not an application, and a
shell that changed size with this would take the Settings page being read with
it — and neither are windows under Xwayland, which have no per-surface scale to
be told about.

## `[keybindings]`

Keys are `Modifier+Modifier+Keysym`. Modifiers are `Ctrl`, `Alt`, `Shift`,
and `Super` (also spelled `Logo`, `Meta`, or `Mod4`), all case-insensitive.
The final component is an xkb keysym name such as `Q`, `F1`, `Return`, or
`BackSpace`.

Actions:

| Action                | Effect |
| --------------------- | ------ |
| `quit`                | Shut the compositor down. |
| `close`               | Ask the focused window to close. |
| `spawn:COMMAND`       | Run a command. Quoting follows shell rules. |
| `vt:N`                | Switch to VT `N`. Ignored when nested. |
| `focus-next-output`   | Move focus to the next output. |
| `focus-prev-output`   | Move focus to the previous output. |
| `move-to-next-output` | Send the focused window to the next output. |
| `cycle-window`        | Rotate the window stack on this output. |
| `switch-window`       | Walk the session shell's window deck one card on, with the modifier still held; letting it go takes the card. Rotates the stack, as `cycle-window` does, where the shell cannot draw a deck. |
| `switch-window-back`  | The same walk, the other way. |
| `guide`               | Show the session shell's guide overlay. |
| `keyboard`            | Show the session shell's on-screen keyboard. |
| `screenshot`          | Photograph the display the user is on. |
| `volume-up`           | Turn the session up one step. |
| `volume-down`         | Turn it down one step. |
| `volume-mute`         | Silence it, or bring it back. Also spelled `mute`. |

Anything defined here replaces the built-in binding for the same key
combination, with one exception: the `guide` chords below cannot be taken over
by another action. Unparseable bindings are logged and skipped rather than
aborting startup.

Letter keys name the physical key, so `Super+Q` matches Q pressed without
shift and `Super+Shift+Q` is a separate binding.

`Any+` in front of a key binds the key rather than a chord: it fires whatever
modifiers are held, and none of them can be spelled beside it. It is for a key
with one job printed on its cap, whose variants elsewhere are all the same job
here — `Any+Print`, so that `Shift+Print`, `Ctrl+Print` and `Meta+Shift+Print`
all take the one kind of picture this shell takes, and the three volume keys,
which mean the same thing however they are reached. Reach for it sparingly: a loose binding on a letter takes that letter
away from every application in the session, in every chord it appears in.
Binding the same key here, decorated or not, takes the whole key back from a
loose built-in — writing `"Print" = "spawn:grim"` leaves `Shift+Print` doing
nothing rather than still photographing the screen.

`guide` is the console "home" button. It is a compositor binding because a
fullscreen application holds the keyboard, so the shell would never see the
key itself; the compositor forwards it over `lxb_shell_v1`. The chords are
`Super+Home` and `XF86HomePage`. It does nothing when no shell has bound that
protocol.

Three more ways in are not chords at all and are therefore not in the table.
The **Windows key on its own** is the home button: it is watched for being
pressed and let go of with nothing in between, because a bare modifier is half
of every `Super+…` binding here and one looked up as a chord would shadow all
of them. Both edges of the key still reach the application, so nothing is left
holding a modifier it is never told about again. The **rear side button of a
mouse** does the same, and is held back from the application entirely, both
edges of it. So does a **controller's Guide or STEAM button**, which the shell
reads from `/dev/input` itself — on its release rather than its press, because
holding it with `R1` is the shell's screenshot chord and a modifier that also
acted on the way down could not be one. A tap is still a tap; a hold spent on
the chord opens nothing.

The chords also outrank every other binding, and cannot be bound to anything
else: the guide is the way back out of whatever is running, so a configuration
file that took its key for something else would leave a session with no way
home. Binding `guide` to a further chord adds it to the protected set rather
than moving it — `"Super+K" = "guide"`, say, makes that chord the home button
too and takes it away from the on-screen keyboard.

`screenshot` is a compositor binding for the same reason `guide` is — the
picture is of whatever is in front, so the key has to work while something is
in front of everything — but it is not protected, and any of its chords can be
given to something else. Its three spellings are the ones hands arrive already
knowing: the Print key under any modifiers, and the Mac chord transcribed onto
a PC keyboard both of the ways it gets transcribed, for a keyboard that has no
Print key at all. It photographs the display holding the keyboard,
writes it into the folder `xdg-user-dirs` records for pictures, and answers
with a flash of that display and the shell's `screenshot.ogg`. Unlike `guide` it
is a *round trip*: the compositor asks the shell where the file should go, so
the key does nothing at all when no shell has bound the protocol.

A controller reaches the same picture without going through this binding at
all. **Guide or STEAM held with `R1`** photographs the display holding control,
and it is the shell's own chord rather than the compositor's, because a pad is
not the compositor's to read: the shell opens it from `/dev/input`, which is
also why the chord works while a game holds everything else. Nothing in this
file changes it.

The three `volume-*` actions are compositor bindings for the reason `guide` and
`screenshot` are, and the plainest of the three: the fullscreen application
holding the keyboard is usually the thing being turned down. They are forwarded
over `lxb_shell_v1` and set nothing themselves — the shell owns the mixer, knows
where the control stands and has somewhere to draw it, so the key does nothing
at all when no shell has bound the protocol. The built-ins are the three keys
with a speaker printed on them, bound with `Any+` for the reason `Print` is: they
are reached through Fn on a laptop and through a media row on a keyboard, and no
two of those agree about what else is held down at the time.

Held down, `volume-up` and `volume-down` go on stepping at the `repeat_rate` and
`repeat_delay` set in `[input]`, because they are keys on the user's keyboard
like any other; `repeat_rate = 0` turns that off as it does everywhere else.
`volume-mute` never repeats: a switch held down is a switch thrown once.

When running nested for debugging, the host compositor's own global shortcuts
win: KDE claims most `Super`+letter combinations, so pick something it does not
use, or drive the overlay with Escape inside the shell instead.

### Built-in defaults

| Binding | Action |
| ------- | ------ |
| `Ctrl+Alt+BackSpace` | `quit` |
| `Ctrl+Alt+F1`…`F12`  | `vt:1`…`vt:12` |
| `Super+Q`            | `close` |
| `Super+Home`, `XF86HomePage` | `guide` (and `Super` on its own, which is not a chord) |
| `Super+K`, `XF86Keyboard` | `keyboard` |
| `Any+Print`, `Ctrl+Shift+3`, `Alt+Shift+3` | `screenshot` |
| `Any+XF86AudioRaiseVolume` | `volume-up` |
| `Any+XF86AudioLowerVolume` | `volume-down` |
| `Any+XF86AudioMute`  | `volume-mute` |
| `Super+Tab`          | `cycle-window` |
| `Alt+Tab` / `Alt+Shift+Tab` | `switch-window` / `switch-window-back` |
| `Super+Left` / `Super+Right` | `focus-prev-output` / `focus-next-output` |
| `Super+Shift+Right`  | `move-to-next-output` |

## Example

```toml
[general]
shell = "lxb-desktop"
output_layout = "horizontal"
output_gap = 0
background = [0.02, 0.02, 0.04, 1.0]

[general.env]
MOZ_ENABLE_WAYLAND = "1"

[input]
keyboard_layout = "pl"
repeat_rate = 30
repeat_delay = 400

# Primary display, pinned at the origin.
[[output]]
name = "DP-1"
mode = "2560x1440@144"
position = [0, 0]
scale = 1.0

# Secondary, placed to its right.
[[output]]
name = "HDMI-A-1"
mode = "1920x1080@60"
position = [2560, 0]

# Never light up the built-in panel.
[[output]]
name = "eDP-1"
enabled = false

[keybindings]
"Super+Return" = "spawn:foot"
"Super+E" = "spawn:lxb-desktop"
"Super+Shift+Q" = "quit"
```

## Per-application settings

`$XDG_CONFIG_HOME/lxb/apps.toml`, written by `lxb-desktop` rather than by
the compositor: it holds the choices the guide overlay makes about one
application, which have to outlive that application running.

Applications are keyed by the name they give themselves — an `xdg_toplevel`'s
`app_id`, or an X11 window's class — never by the window title, which is a
document name and would change under the setting.

```toml
[apps."org.mozilla.firefox"]
# The controller is a mouse inside this application: the right stick aims, `A`
# and `B` (or R3 and L3) click, and the left stick and D-pad scroll. Off unless
# the guide's tile has been switched on.
stick-pointer = true
```

Editing it by hand is fine. The shell reads it once at startup and rewrites it
whenever a setting changes, keeping only the applications something has been
chosen for; an unreadable file is reported and treated as empty rather than
being allowed to stop the session coming up.
