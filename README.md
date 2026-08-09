# LineXinBar

A micro Wayland compositor with first-class multi-display support, plus an
XMB-style shell that runs inside it.

Two binaries:

| Binary        | What it is                                                    |
| ------------- | ------------------------------------------------------------- |
| `lxb`         | The compositor. DRM/KMS on a TTY, or nested inside a desktop.  |
| `lxb-desktop` | The shell: a cross-media-bar launcher, drawn on the GPU.       |

## Why not just Gamescope

Gamescope is single-output by construction: it owns one CRTC and scales one
application onto it. LineXinBar keeps the same deliberately small "one
application fills the screen" model, but every connected display is a real
output with its own CRTC, scanout swapchain and vblank-driven render loop.
Displays with different resolutions and refresh rates therefore run
independently rather than being locked to a shared heartbeat.

## Requirements

LineXinBar is Linux-only, and structurally so: it drives displays through
DRM/KMS, reads input through libinput, and asks a seat manager for the device
handles that both of those need. Nothing about it is hardware-specific — every
connector, mode, GPU node and capability is discovered at runtime — but the
kernel interfaces underneath it have no portable substitute.

### The session

- **Linux with DRM/KMS.** The `udev` backend becomes DRM master for the card
  and drives every connected connector itself.
- **A seat manager: systemd-logind, or `seatd`.** The compositor never opens
  `/dev/dri/*` or `/dev/input/*` directly; libseat hands it the file
  descriptors and takes them back across a VT switch. Having neither running
  is the usual reason a start from a TTY fails.
- **udev**, for enumerating devices and hearing about hot-plug.
- **Atomic modesetting — for HDR only.** The colour pipeline is one atomic
  commit, so a driver stuck on the legacy API reports HDR as unsupported and
  everything else carries on. See [Display](#display).

### Libraries

| Library                                  | For                                       | Needed by  |
| ---------------------------------------- | ----------------------------------------- | ---------- |
| `libwayland-server` / `libwayland-client` | The protocol itself, via the system C library rather than a Rust reimplementation | both |
| `libxkbcommon`                            | Keymaps, on both sides of the keyboard grab | both     |
| `libudev`                                 | Device enumeration; gamepad hot-plug in the shell | both |
| `libinput`                                | Pointers, keyboards, touchpads             | compositor |
| `libseat`                                 | Session and device handover                | compositor |
| `libdrm` + `libgbm`                       | Modesetting and scanout buffers            | compositor |
| `libEGL` (+ a GLES driver)                | The compositor's renderer                  | compositor |
| `libvulkan` **or** `libEGL`               | The shell's renderer, whichever is present | shell      |

The X libraries — `libX11`, `libxcb`, `libXcursor`, `libXi`,
`libxkbcommon-x11` — are pulled in by the nested `winit` and `x11` backends
and by XWayland. They are not needed to run on hardware, but they are linked
into the binary either way.

### The GPU

The compositor needs GBM and an EGL/GLES driver; Mesa provides both for AMD
and Intel. The shell asks wgpu for Vulkan or GL, with no optional features and
downlevel default limits, which is deliberately modest — integrated and fairly
old GPUs qualify.

The rough edge, as for every GBM-based compositor, is the Nvidia proprietary
driver: it needs a recent version with GBM support and
`nvidia-drm.modeset=1`. That combination has never been tried here.

### Optional at runtime

Each of these is looked for when it is wanted, and its absence costs exactly
one feature:

| Program                                   | Gives                                  | Missing                                        |
| ----------------------------------------- | -------------------------------------- | ---------------------------------------------- |
| `Xwayland`                                | X11 applications                       | A Wayland-only session; logged, never fatal    |
| `dbus` (`dbus-update-activation-environment`) | D-Bus activation inside the session | Activated apps may appear on the outer desktop |
| `wpctl` / `pactl` / `amixer`              | The volume bar, in that order of preference | No volume bar                             |
| `ddcutil`                                 | Brightness for external monitors, over DDC/CI | Brightness only where the kernel has a backlight |
| `xdg-open`                                | Opening one of the user's own files when nothing installed declares its type | Those rows are listed but report that nothing opens them |
| `ffmpegthumbnailer` **or** `ffmpeg`       | A frame of each film, on its row in Video | Films keep the film-strip glyph; photographs are unaffected |

### Permissions

- **A seat.** A logind session, or membership of whatever group `seatd` was
  built to accept.
- **The backlight**, to dim a built-in panel: write access to
  `/sys/class/backlight/*/brightness`, which is normally the `video` group
  plus the udev rule the distribution ships. LineXinBar checks it can write
  before offering the bar at all, so a missing rule shows up as no brightness
  control rather than as a slider that does nothing.
- **i2c**, for `ddcutil` to reach an external monitor — usually the `i2c`
  group.

### Bundled, so not required

Roboto and a subset of Bibata Modern Classic ship in the tree and are compiled
in or loaded from `share/`. LineXinBar does not depend on what fonts or cursor
themes a console has installed.

### What this has actually run on

One machine: a single AMD GPU on `amdgpu`, driving two external displays, plus
nested sessions under `winit` and the X11 backend. Nothing is pinned to that
hardware, and the multi-GPU path exists, but no second GPU and no other vendor
has ever been in front of it. Everything above is what the code *needs*; only
that one configuration is what it is *known* to work on.

## Building

A Rust toolchain — 1.85 or newer, as `rust-version` records — plus
`pkg-config` and the development headers for the libraries above.

```sh
cargo build --release
```

Arch (headers ship in the same packages):

```sh
pacman -S --needed rust pkgconf wayland libinput seatd systemd-libs libdrm \
    mesa libxkbcommon libglvnd libx11 libxcb libxcursor libxi libxkbcommon-x11
pacman -S --needed xorg-xwayland dbus wireplumber ddcutil   # optional
```

Debian and Ubuntu:

```sh
apt install build-essential pkg-config cargo libwayland-dev libinput-dev \
    libseat-dev libudev-dev libdrm-dev libgbm-dev libxkbcommon-dev \
    libegl1-mesa-dev libx11-dev libxcb1-dev libxcursor-dev libxi-dev \
    libxkbcommon-x11-dev
```

Fedora:

```sh
dnf install cargo pkgconf-pkg-config wayland-devel libinput-devel \
    libseat-devel systemd-devel libdrm-devel mesa-libgbm-devel \
    libxkbcommon-devel mesa-libEGL-devel libX11-devel libxcb-devel \
    libXcursor-devel libXi-devel libxkbcommon-x11-devel
```

Package names drift; the library list above is the thing to match if yours
disagrees. The shell also wants a Vulkan driver at runtime where one exists
(`vulkan-radeon`, `vulkan-intel`, `mesa-vulkan-drivers`), and falls back to GL
where it does not.

## Running

### Nested, for development

The compositor appears as an ordinary window in your existing session, so it
can be started and killed without touching a TTY.

```sh
./target/release/lxb --backend winit
```

It prints the Wayland socket it created. Point clients at it:

```sh
env -u WAYLAND_SOCKET -u DISPLAY WAYLAND_DISPLAY=wayland-1 foot
```

### Nested with several virtual displays

The winit backend can only ever open one window. To exercise the
multi-display paths without owning extra monitors, the X11 backend opens one
window per virtual output (Xwayland is fine):

```sh
./target/release/lxb --backend x11 --outputs 3 --window-size 800x600
```

Each window is a real output with its own position in the logical layout,
which clients see through `wl_output` and `xdg-output`.

### Native, on hardware

From a TTY, with `seatd` running (or logind):

```sh
./target/release/lxb --backend udev
```

Every connected connector becomes an output. `Ctrl+Alt+F1`…`F12` switch VTs.

`--backend auto` (the default) picks `winit` when a session is already
running and `udev` otherwise.

### As a session

```sh
lxb --shell
```

That is the whole thing, and it is what to run from a TTY. `--shell` starts
`lxb-desktop` and ties the compositor's lifetime to it, so quitting the shell
logs you out rather than leaving an empty compositor with no way out of it.
Failing to start the shell is fatal, for the same reason.

A bare program name is looked for next to the `lxb` binary before `PATH`,
so a build tree runs its own matching shell:

```sh
./target/release/lxb --shell
```

Set `general.shell` in the config to run something else. To have a display
manager offer it, install [`share/wayland-sessions/lxb.desktop`](share/wayland-sessions/lxb.desktop)
into `/usr/share/wayland-sessions/`.

For anything besides the shell, `general.autostart` and a trailing command
both still work:

```sh
./target/release/lxb -- foot
```

Those are unsupervised: the compositor keeps running when they exit.

For a complete desktop session, start the compositor on its own session bus:

```sh
env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u DISPLAY \
  LXB_PRIVATE_DBUS=1 dbus-run-session -- \
  ./target/release/lxb --shell
```

The `scripts/run-nested.sh` development helper does this automatically.

LineXinBar pins autostarted processes to its own `WAYLAND_DISPLAY` and removes
any inherited host `WAYLAND_SOCKET` and `DISPLAY`. If its private XWayland is
available, `DISPLAY` is then replaced with that server's value. The development
`scripts/run-nested.sh` helper starts the shell through this same boundary.
This is particularly important for the X11 nested backend: its outer host
windows are unrelated to the private display offered to applications.
Before launching the shell, LineXinBar also updates its marked private D-Bus
daemon with those private display names. D-Bus-activated GUI applications
therefore enter LineXinBar instead of inheriting the outer desktop.

X11 applications are not merely redirected to a socket: LineXinBar owns the
XWayland process and its X window manager. X11 toplevels are tiled, rendered,
focused, closed, and moved between outputs through the same compositor paths as
native Wayland windows. Clipboard and primary-selection transfers work in both
directions.

## The shell

`lxb-desktop` binds `zwlr_layer_shell_v1`, so it is not tied to LineXinBar —
it runs on any compositor implementing layer-shell, which also makes it
debuggable on its own.

The bar opens with Settings, LineXinBar's own column, which holds the shell's
settings the way the XMB's Settings region held the PS3's. Everything after it
comes from `.desktop` files in the usual XDG search path, grouped into the
categories Plasma's launcher uses: System, Multimedia, Graphics, Internet,
Office, Games, Development, Education & Science, Utilities, and Other. A
category with no application in it is hidden — except Settings, which is part
of the bar rather than a result of what is installed.

Multimedia carries two subcategories of its own, Music and Video, and Graphics
carries one, Images. What is in them is the user's own files rather than
applications. No `.desktop` file can say which half of Multimedia an
application belongs to — the menu spec requires `AudioVideo` alongside `Audio`
or `Video` but never the reverse, so an entry may declare `AudioVideo` and
stop, and many of the best-known media applications do exactly that — so the
players and the editors stay in their columns, where nothing has to be guessed
about them.

### Music, Video and Images

All three rows list what is on the disk: every audio, video or image file
anywhere under `$HOME`, alphabetically, with the folder it came from written
under its name. A file's kind is its extension, and its row opens it in
whatever the user has already set as their handler for that type — the default
from `mimeapps.list`, then any installed application that declares the type,
preferring one that calls itself a `Player` or a `Viewer` over one that calls
itself an editor, then `xdg-open`. So a song opens in mpv rather than Audacity
and a photograph in Gwenview rather than GIMP, without the user having chosen
either.

One walk fills all three: the expensive half is reading the directories, and
what is in them decides which row a file lands on. It runs on a worker thread
from the moment the shell starts and what it finds is hung on the bar as it
arrives, so stepping into Music never waits on the disk. It repeats every five
minutes, which is how a file copied in during a session turns up and how one
deleted goes away again. Hidden folders are skipped, as every desktop's
indexer skips them, and symbolic links are not followed.

A row holds everything found, however much that is. Nothing about a frame the
shell draws depends on how much music somebody owns: a column is a screen tall,
so the drawing and the hand both walk the rows that can be on the display and
never the list, and a picture is made only for the handful either side of the
cursor.

The shelves live on the worker, not in the shell. It walks, it keeps what it
finds, it puts the chosen order on, and it builds the rows themselves — and
what crosses back is a finished list, which the shell swaps into the bar in a
few microseconds however long it is. Everything that costs anything in
proportion to the size of a collection happens on that side of the wire: the
merge, the sort, letting go of the list that was replaced. The shell sends
three things the other way — list this shelf differently, this file has been
deleted, and here are the rows you can let go of now — and holds no files at
all.

This is what the first two seconds of a session are made of. On the home
directory it was written against, half a million pictures, the shell used to
spend about 1.4 of those two seconds blocked: a quarter of a second to rebuild
the rows, four times a second, on the thread that draws. It now spends four
microseconds per delivery and drops no frames. Deliveries also thin out as the
shelves grow, since a shelf that takes a tenth of a second to build is not
worth rebuilding four times a second for one more file nobody could pick out of
half a million — the walk hands over eight times instead of fifty-seven.

Everywhere under `$HOME` means everywhere: a repository checked out in the home
directory has its screenshots and its SVGs listed like anything else, since
nothing about where a file sits says it is not a picture. Icon and dump formats
(`.ico`, `.xpm`, `.pbm`) are left out, and so are `.ts` and `.mts` — both name
MPEG transport streams and both name TypeScript sources, and a development
machine would otherwise file several thousand of the second kind under Video.

#### Thumbnails

Video and Images draw the files themselves rather than a mark standing for
them: a row is a card with a frame of the film or the photograph on it, fitted
to its own shape so nothing is cropped and a portrait picture stays portrait.
Music does not, because getting cover art out of an audio file means parsing a
tag format per container — and a track is picked by its name anyway.

Nothing is made ahead of time. Each display asks for the rows within four of
its cursor, once a frame; two workers make those and nothing else, and the
atlas holds only what is on screen. Scrolling a thousand photographs therefore
costs the same as looking at six, and a library of any size costs nothing at
all until somebody opens the column.

What is made is written to `$XDG_CACHE_HOME/thumbnails/large`, in the layout
the freedesktop thumbnail specification lays down — a PNG named for the MD5 of
the file's URI, carrying the source's URI and modification time so a stale one
can be told from a good one. That is the same cache every file manager fills,
so on a machine where the user has browsed their pictures in Dolphin they are
already there, and the ones this shell makes are still there afterwards.

Photographs are decoded in-process (PNG, JPEG, GIF, WebP, BMP, TIFF, and SVG
through the same renderer the shell's own glyphs use). Films need
`ffmpegthumbnailer` or `ffmpeg`; without either, and for a format nothing here
decodes — AVIF, HEIC, camera raw — the row keeps the column's glyph.

A machine with music on it but no media player installed gets its Multimedia
column back the moment the first file is found, and the same goes for Graphics
and a photograph: a column is hidden for having nothing in it, not for having
no applications in it.

#### What the menu over a file offers

`Y` on one of these rows raises a menu of five, in two bands — three that act
on the file and two that do not:

**Open** starts it in whatever a plain `A` would have used. **Open with** lists
every installed application that says it handles the type, best first, each
under its own name and its own icon; the one at the top is the one Open would
take, and wears the same tick the Settings column puts on a value in force. The
row is greyed where nothing installed claims the type, since there is then
nothing to choose between.

Choosing one of them opens nothing. It records that application as the default
for the type — written into the user's own `mimeapps.list`, where every other
desktop keeps it, so it holds for the file manager and the browser too and it
holds after a restart. Open is what acts on it, which is the whole difference
between the two rows: one plays the file, the other answers "which program
plays these", and a list that did both would make changing the answer cost a
window every time.

The list stays up, and the tick moves to the row that was pressed. It is a
setting being made rather than a command being run — the same bargain the
mixer's tracks strike, where the row changing *is* the answer — so it is the
user's to leave, by Cancel or `B`, when they are satisfied. The rows keep the
order the panel opened in while it is up, even though the newly chosen one now
belongs at the head: a list that reordered itself under a press would move the
row somebody was already reaching for.

**Delete** asks first, and what it does is move the file to
`~/.local/share/Trash` in the layout the freedesktop trash specification lays
down — the same trash Dolphin, Nautilus and `gio trash` fill, so a file put
there by this shell can be restored from any of them by somebody who has never
heard of LineXinBar. A file on a volume mounted inside `$HOME` goes to a trash
at the top of *that* volume, because a rename cannot cross a filesystem and
copying forty gigabytes to delete it is not a deletion. Nothing is ever
unlinked and nothing is ever copied; if the move fails the file stays exactly
where it was and the panel says so. The row is greyed for a file outside the
user's home directory. The row it was on goes at once rather than at the walk's
next pass, so the bar is never still offering to play something the user has
just watched themselves delete.

Below the rule, **Sort** and **Cancel**. Sort is about the column rather than
the file, which is what the rule is saying.

#### What order the rows are in

Alphabetical to begin with, and nine orders altogether: name either way, size
either way, type, created either way and modified either way. Chosen from the
Sort row and remembered per shelf in `[media-sort]` of the shell's settings
file, because how somebody wants their music listed says nothing about how they
want their photographs listed.

Size and both dates are read once, when the walk first sees a file — one
`stat` per media file per session, and none at all on the passes after. Type is
the mime type rather than the extension, so `.jpg` and `.jpeg` are one group.
A file the disk knows no date for sorts last in *both* directions of a date
order, because "no date" is not an early date; and an order the whole shelf has
nothing to answer with is drawn greyed rather than offered and silently doing
nothing — several filesystems keep no creation time at all.

The order is put on when the rows are drawn, not when the shelf is filled. The
shelf itself is always alphabetical, because that is the order it is merged in:
what the walk finds arrives in whatever order the filesystem answered, and
merging a batch into a list already in the same order is one pass over both
rather than a re-sort of everything.

Choosing an order shows the column from its first row. Somebody who has just
asked for the newest first is asking to be shown the newest, and a cursor held
on whichever file it happened to be standing on would answer with that file's
new position instead. Only the display the order was chosen on moves; any other
screen showing the same shelf keeps its file, because the order changed
underneath it rather than at its request.

### Subcategories

A column is a tree, not a list. A row can be a subcategory, and choosing one
opens its own column beside it: the bar slides over, the column behind keeps
only the row it was opened from, and what is left on screen is where the user
came from, reading left to right under the category it hangs off.

```
Settings
   |
Appearance  >  Accent color
```

Stepping further in slides the whole chain along by one, so the column just
opened stands where the cross always is and the one it came out of takes the
place its own parent had. A path four subcategories deep therefore costs no
more of the screen than a path one deep — the far end of it goes off the side
rather than the near end marching across.

What is behind does not merely dim, it *recedes*: one step further back per
column opened in front of it, drawn smaller and hazier each time, with the
category row a step behind the outermost column of all. One subcategory in,
the row is two steps back and its column one, so how far the path has gone is
answerable at a glance and not only by reading it.

That is what the Settings column is made of, so it stays a few rows deep
instead of becoming one long list. Right steps further in and Left steps back
out — but only once there is a path to walk. At the top of a column the two
still belong to the category row, because a column of nothing but
subcategories would otherwise be one you could never get past.

A list of values opens on the value in force and marks it, so what the shell is
set to is where the cursor already is. Highlighting another previews it; the
palette flows into that value while the mark and saved setting stay where they
were. Pressing accept moves the mark, applies the value and remembers it in
`~/.config/lxb/shell.toml`. Stepping back or left without accepting flows
back to the applied value instead.

### Appearance

`Settings > Appearance > Accent color` is the shell's own colour: **Purple**,
which is what it comes up in, plus **Blue**, **Green**, **Yellow**, and **Red**.
Each is drawn in itself, and the one in force carries a tick.

An accent is a whole palette rather than one colour. Previewing one smoothly
changes the selection glow, the lit rim of a chosen pane, the glass and the
bloom under the icon the cursor is on — and the wallpaper with them, since its
ribbons and aurora are drawn *in* the accent, and a sky that no longer belonged
to it would read as two themes fighting. Nothing is reloaded to make that
happen: every colour in the shell is read as it is drawn, so every display
travels through the same transition at once.

It is written to `~/.config/lxb/shell.toml`, which is a file you can also
just edit:

```toml
accent = "Blue"
```

An unknown name there is reported and ignored, and the shell comes up violet.

Entries are read directly rather than through the XDG menu files, so the
`.menu` layouts a desktop ships (Plasma's vendor submenus, its Lost & Found, and
anything kmenuedit has rearranged) do not carry over. `OnlyShowIn` and
`NotShowIn` are honoured against `XDG_CURRENT_DESKTOP`, which the session sets
to `Lxb`: an entry written for one specific other desktop stays in that
desktop's menu.

### Display

`Settings > Display` is the one part of the Settings column the shell does not
carry out itself. It sends what was chosen over `lxb_shell_v1` and the
compositor does the work, because none of it is a client's to touch. There are
four pages: **Resolution**, **Refresh rate**, **Orientation**, and **HDR**.

Every one of them is *per screen*, and every one of them names the screen
before it offers anything — see below, where the rule is written out once for
HDR and holds for all three.

#### Resolution and refresh rate

```
Settings > Display > Resolution    >  DP-1  >  2560 × 1440
Settings > Display > Refresh rate  >  DP-1  >  144 Hz
```

Two rows rather than one list of modes, because they are two questions. A
television reports the same six sizes at four rates each, and a single list
would be two dozen rows most of which repeat the size above them. So the sizes
are listed once each, saying what the best rate at that size is, and the rates
are asked for separately.

**Every size the connector lists is on the first page.** The second page is a
page about the size that screen is set to, and lists **every rate that size
carries** — no more, because a refresh rate is not something a display has on
its own. It is something a *mode* has: a monitor that does 240 Hz at 1440p and
120 Hz at 1080p does not do 240 Hz at 1080p, and a page offering it there would
be offering a mode the connector has never listed. Pick the size first and the
rate page follows it.

Which means only one of the two moves the other:

- A size takes the current rate with it where the new size carries it, and asks
  for the fastest of that size where it does not.
- A rate never moves the size. It sets the rate, at the size it was listed
  under.

Neither ever asks for a mode the connector has not listed, and every row does
what its title says.

Two of a display's modes can round to the same readable rate — 119.998 and
120.000 are both "120 Hz", and high-refresh panels list both. Neither is
dropped: where the short form would collide, every rate in the collision is
printed to the thousandth of a hertz it is counted in.

Every screen the compositor reports modes for is listed on both pages, even one
with a single mode: that row is what says what that screen is showing. On the
rate page it names the size as well (*120 Hz at 3840 × 2160*), because the
rates underneath it are that size's. A nested session reports no modes at all,
because the size of its window belongs to the compositor LineXinBar is running
inside; where nothing reports any, the row says so rather than opening onto an
empty column.

The mark is on what the display is **actually** running, not on what was last
asked for: the answer comes back over the same protocol in the same breath as
the change, and a mode the hardware refused must not read as chosen.

#### Orientation

```
Settings > Display > Orientation  >  DP-1  >  90° Rotation
```

For a screen standing on its side. Four turns, on every screen the compositor
turns itself:

| | |
| --- | --- |
| **0° Rotation** | Landscape, the way the display is built. |
| **90° Rotation** | Portrait, for a screen turned clockwise. |
| **180° Rotation** | Landscape, for a screen hung upside down. |
| **270° Rotation** | Portrait, for a screen turned the other way. |

Each row is a *drawing* of the monitor stood that way, its stand saying which
way up — the same monitor the screen list beside it is drawn with. They are the
only values in the Settings tree with pictures of their own: a brightness in
cd/m² has no shape, and inventing one would be drawing a picture of a number,
but an orientation is a shape, and the row matching the screen in front of you
can then be picked without reading it. The degrees are what the drawing cannot
say — which of the two portraits this is, and how far from where the display
started.

Unlike a mode, none of these is a list the display has: nothing here reaches
the connector. The picture is composited turned and scanned out at the mode's
own pixels, which is why no hardware can refuse a turn and why every screen is
offered all four. What a quarter turn does move is everything else — the
display's logical width and height swap, so the screens laid out beside it
shift along, the bar and every other layer surface are re-arranged to the new
shape, and every window on it is re-tiled. That is the whole reason it is the
compositor's to do.

**90°** is the turn a screen swivelled clockwise wants — the picture is drawn a
quarter turn anticlockwise, which stands up on a monitor whose foot has gone to
the left — and **270°** is the other one. The four mirrored orientations
`wl_output` also has are not offered: a mirrored picture is a projector rig
rather than a way up. The compositor's own config can still set one, and a
screen in one is named in the row above the list with none of the four marked,
because it is in none of them.

Which screens are listed is the compositor's answer rather than a guess: it
reports an orientation for every display it turns itself, and for no others. A
nested session is the ordinary "no others" — the way up of its window belongs
to the compositor LineXinBar is running inside — and where nothing reports one,
the row says so instead of opening onto an empty column.

#### HDR

`Settings > Display > HDR` turns high dynamic range on. Both halves of it are a
modesetting client's: the connector has to be told the signal is BT.2020 with
the ST 2084 (PQ) transfer function, and the CRTC's colour pipeline —
`DEGAMMA_LUT` → `CTM` → `GAMMA_LUT` — has to re-encode the composited sRGB
picture into it. Without the second half, a display told to read sRGB numbers
as PQ shows a scene far too dark and the wrong colour.

On a desk with more than one HDR screen, it names the screen first:

```
Settings > Display > HDR  >  DP-1  >  SDR brightness  >  250 cd/m²
```

Every screen that can be driven in HDR is listed under it, by the connector
name the compositor logs at startup, with what it is doing and the peak it
reports. Screens appear when they are plugged in and disappear when they are
not; nothing has to be configured for a new one to show up, and nothing in the
shell knows the name of any particular display.

The screen comes before the settings because the settings are per screen, all
the way down to the file they are written to. An HDR television beside an SDR
laptop panel is the ordinary case, and one set of answers for both would be
describing a machine nobody has. Unplugging a display does not forget what it
was set to; it comes back the way it was left, because both halves file the
settings under the connector.

With **one** HDR screen there is nothing to choose between, so that step is not
there:

```
Settings > Display > HDR  >  SDR brightness  >  250 cd/m²
```

A question with a single answer is not a question, and a level that exists only
to be walked through reads as though something else were on offer. The screen
is named in the HDR row's own subtitle instead, so it is still clear what the
settings belong to. Screens that cannot do HDR are what decides this as much as
screens that can: one capable screen beside three SDR ones still collapses,
because the list would have had one row. A session where *none* can shows a
single row saying so — a subcategory you cannot step into is worse than one
that explains itself.

| Setting | |
| --- | --- |
| **HDR** | Off, or on. Refused, and reported as such, unless the display's EDID advertises PQ and the driver has a colour pipeline to feed it. |
| **SDR brightness** | What plain white is sent at, 80 to 400 cd/m². |
| **sRGB color intensity** | 0% to 100%: how far sRGB's colours are stretched into BT.2020's much wider gamut. |
| **Peak brightness** | What the display is told to expect, or *Display default* — the peak it reports in its EDID. |

**SDR brightness** is the control that matters most, because nothing LineXinBar
composites is HDR content: every application and the shell itself are SDR, so
this alone decides whether the session comes out dim or blinding. sRGB's own
reference is 80 cd/m², which describes a darkened grading suite; the default is
200, which is roughly an ordinary lit room.

**sRGB color intensity** is the choice between the two honest things to do with
an sRGB picture inside BT.2020. At **0%** its colours are placed where they
actually belong, so the session looks exactly as it did in SDR. At **100%** the
numbers are sent through untouched, so sRGB's red is displayed as BT.2020's red
and everything comes out far more saturated — the "vivid" mode a television
ships in. In between is a blend of the two, and because both ends are matrices
whose rows sum to one, so is everything between them: white stays white at
every setting.

It is the one control here that some hardware cannot honour, and the one that
says so. It is the `CTM`, and a matrix is only a gamut conversion when it acts
on linear light — so it needs a `DEGAMMA_LUT` in front of it, which not every
display engine exposes (amdgpu on RDNA 4 does not, on its display pipes). Where
there is none, the sRGB decode is folded into the gamma curve so brightness
stays right, the matrix is left at identity, and the gamut sits at the vivid
end. On such a screen this row offers no choice at all: it reads *Not available
on this display*, and opening it gives the reason. The compositor tells the
shell which displays can do it, per display, so the answer is right on a
machine where one screen can and another cannot.

Everything on the page is applied as one atomic commit, which the driver is
asked to validate before it is made for real — a pipeline the hardware will not
take is refused by a commit that changed nothing, and the page reports the
display as still in SDR rather than showing the switch as having taken. Leaving
the session puts every display back into SDR first, so quitting always returns
a readable console.

Turning HDR **off** is a signal in its own right, not the absence of one. The
display is sent the same metadata infoframe carrying the traditional-gamma
EOTF, which is how CTA-861.3 has a source say the picture is ordinary again; a
sink that merely stops hearing about HDR has been told nothing, and one that
goes on decoding PQ shows an SDR picture as very little at all. Because those
properties can force a modeset, the compositor also re-reads the CRTC
afterwards and draws a full frame, rather than trusting that what it last put
on screen survived the driver rebuilding the pipe.

Each screen says what it is doing — *Ready* or *In HDR*, and the peak it
reports — in its own row where there is a list, and in the HDR row's subtitle
where there is not. Either way the state of the session is answerable without
stepping into anything. It carries no
value, so choosing it does nothing — a row that describes something true must
not be one the user can un-choose. A switch that does nothing has to have its
reason on the same page, not in a log.

Unlike the accent, none of this previews as the cursor passes over it.
Reconfiguring a connector cuts most displays to black for a second while the
panel resynchronises, and a setting that blacked the screen out every time the
cursor moved down the list would be unusable. Walking the list is an invitation
to read the names; the value is committed when it is chosen.

All of it is written to `~/.config/lxb/shell.toml` beside the accent, one
section per connector:

```toml
accent = "Blue"

# What a display with no section of its own gets, including one plugged in
# for the first time. The mode has no such default: it names something one
# connector offers, and the display beside it may not offer it at all.
hdr = false
hdr-sdr-brightness = 200

[display.DP-1]
mode = "2560x1440@144"
transform = "90"
hdr = true
hdr-sdr-brightness = 250
hdr-srgb-intensity = 50
hdr-peak-brightness = 0
```

`mode` holds both halves of the page, spelled and named as the compositor's own
config spells and names a mode, so a line can be moved between the two files
and mean the same thing. Without an `@rate` it asks for the fastest mode of
that size. `transform` is the Orientation page, spelled the same way and for
the same reason: `normal`, `90`, `180`, `270`, or one of the four mirrored
spellings the page does not offer. A display with no `mode` or no `transform`
line is left at whatever the compositor brought it up at.

The compositor's own `config.toml` has the same mode, the same transform and
the same four HDR settings per output, for a session with no shell — see
[docs/configuration.md](docs/configuration.md).

### Several displays

Each display gets its own bar, not a copy of one: they browse independently and
remember where they were. Only one takes input at a time — the others are
dimmed and drop the footer, so it is obvious which one the controller is
driving. `L1` / `R1`, or `Tab` / `Shift+Tab`, hand control to the next display;
the compositor's first output has it at startup, since Wayland has no notion of
a primary display.

An application launched from a display opens on that display, because the shell
names that display with `set_launch_output` before starting anything. It does
not leave the compositor to work it out from keyboard focus: focus has usually
moved on to something else by the time the new window maps.

And it stays there. The display a window is placed on is recorded on the window
itself, so every later re-tile — a sibling window closing, the client asking to
be maximized or fullscreened, an X11 client moving itself — puts it back on the
same screen. Only `Super+Shift+→` moves a window between displays. Without that
record the placement would be re-derived from the window's bounding box, which
a window that has mapped but not yet drawn does not have: an application whose
updater or splash window closes as the real one appears would arrive on the
first display instead of the one it was started from.

Only the catalogue of applications and the running processes are shared. The
device, icon atlas, shaders and shaped-glyph cache are shared too, so a second
monitor costs one more swapchain rather than a second copy of everything.
Hotplug is followed in both directions; unplugging the display that had control
passes it to a neighbour.

| Input                                      | Action                              |
| ------------------------------------------ | ----------------------------------- |
| `←` / `→`, D-pad or left stick left/right  | Change category                     |
| `↑` / `↓`, D-pad or left stick up/down     | Change row                          |
| `Enter`, controller `A` or `Start`          | Launch, open a subcategory, or choose a value |
| `→` inside a subcategory                    | Open the subcategory under the cursor |
| `←` inside a subcategory                    | Step back out one level             |
| `Tab` / `Shift+Tab`, `L1` / `R1`            | Move to another display             |
| `Esc`, `Backspace` or controller `B`        | Step out, then open the guide overlay |
| `Home`, `Super`, mouse side button, controller Guide/STEAM button | Open the guide overlay |
| `Y`, `F10`, right mouse button, controller `Y`/`Triangle` | Open [the context menu](#the-context-menu) on what is selected |

Keyboard navigation also accepts the keypad arrows, WASD, and HJKL. Held
directions repeat after a short delay; the analogue stick uses a dead zone
with hysteresis so drift near its edge cannot rapidly change selection.

Controllers are discovered and hot-plugged directly through the Linux gamepad
API, using SDL-compatible mappings (including `SDL_GAMECONTROLLERCONFIG`, as
used by Steam). A pad the mapping database does not know still works: buttons
it cannot name fall back to their raw Linux codes, so `BTN_SOUTH` launches and
`BTN_EAST` goes back whatever the database thinks.

The two middle face buttons are the awkward case, because the naming that comes
back for an unmapped pad is positional rather than physical — the same
`Button::North` is the button with `X` on it under one driver and the one with
`Y` on it under the next, and the raw code says no more than the name does. So
an unmapped pad has *both* of them raise the context menu, and both spell the
keyboard chord when Select is held with them. Nothing else on the shell's own
screens wants either button, so the cost of guessing is one extra way into a
menu; the cost of refusing to guess was a controller with no context menu at
all. A pad the database does know is read by name only, and there `X` alone
stays the running application's.

Every button press is logged at debug level with its mapped name, its raw code
and which of the two namings it came under, which is the fastest way to work
out what an unusual pad is actually sending:

```sh
RUST_LOG=lxb_desktop::controller=debug lxb-desktop
```

Controller initialisation failure is non-fatal and leaves the keyboard usable.
Pass `--no-gamepad` to skip controller discovery entirely — which also turns
off [the stick pointer](#the-stick-pointer), since there is no stick to read.

Icons are resolved through the freedesktop icon theme spec, following
`Inherits` from `index.theme` and falling back to hicolor and
`/usr/share/pixmaps`. Both PNG and SVG are supported, including extensionless
absolute paths used by AppImages and non-standard theme directory layouts.
Broken high-priority theme entries fall through to inherited themes, and a
generic application icon is used when a desktop entry provides no usable icon.

## The guide overlay

A launched application fills the screen and owns the keyboard, so there has to
be a way back out of it that the application cannot swallow. Pressing the
controller's guide button, or the compositor's `guide` binding, brings up a
menu over whatever is running:

| Entry             | Effect |
| ----------------- | ------ |
| Stick pointer     | Whether the right stick is a mouse inside the application in front |
| Volume mixer      | How loud that application is, on its own. Not built yet |
| Volume            | How loud the session is — a bar, moved with Left/Right; `A` mutes |
| Brightness        | How bright *this* display is, where that can be changed |
| Resume            | Dismiss the overlay |
| Close *app*       | Ask the application to close, so it can prompt about unsaved work |
| Dashboard         | Show the bar over the running application, without closing it |
| Power             | Suspend, turn off, or end the session |

Close and Dashboard are offered only while something is running. Nothing but
the power button ends the session: `Esc` opens this menu rather than quitting,
so leaving is always a deliberate choice.

The first two are square tiles sharing one line at the head of the column,
because they are switches rather than rows: Up and Down treat the pair as one
stop on the way down the column, and a switch that cannot be thrown from where
the user is standing is stepped over rather than stopped on.

The header is the wall clock, and under it whatever is running on this display.

### Quick settings

The two bars go through the interfaces that exist below a desktop, because in
this session there is no desktop to ask — no settings daemon, no applet, no
`org.kde.*` or `org.gnome.*`.

- **Volume** is whichever of WirePlumber (`wpctl`), PulseAudio (`pactl`) or the
  kernel mixer (`amixer`) answers first, in that order. The last needs no sound
  server running at all.
- **Brightness** is the kernel's backlight class for a screen wired into the
  machine, and DDC/CI over the monitor's own i2c bus (`ddcutil`) for one that is
  plugged into it. The bar belongs to the display the menu is on, matched by DRM
  connector, so the monitor whose brightness moves is the one being looked at.

Either bar is left out of the column entirely when nothing on the machine can
move it, rather than drawn dead. A backlight is only offered if it can actually
be written to, which usually means being in the `video` group; DDC/CI usually
means the `i2c` group and the `i2c-dev` module.

All of it runs on a worker thread. A monitor can take half a second to answer
on i2c, and the bar moves the moment a key is pressed rather than waiting for
it.

Set `LXB_BACKLIGHT` to a directory under `/sys/class/backlight` to name
the backlight device outright, for the machines where two devices describe one
panel or where the screen cannot be recognised as built in.

The overlay is drawn by the shell, not the compositor, so it uses the same
renderer and fonts as the bar. It moves its layer surface to the overlay layer
with `Exclusive` interactivity and draws onto a transparent surface, which is
what lets the running application stay visible through the scrim.

There is one overlay, on the display holding control — the other displays carry
on showing their own bar. `L1` / `R1` and `Tab` / `Shift+Tab` work from inside
the menu, and it follows control to the next display; with more than one
display it names the one it is on.

Everything it says is about that display: it offers to resume or close the
application running *there*, and reads *Nothing is running* on a display that
has none, whatever is on the others.

Two input paths reach it, because neither alone is enough:

- **Controllers** are read straight from `/dev/input` rather than through
  Wayland, so the guide button arrives even while a game holds the keyboard —
  as does the on-screen keyboard's chord, for the same reason. Every other
  control is ignored in that state, so the bar cannot react behind a running
  game.
- **Keyboards** go to the focused application, so the shell would never see the
  key. The compositor therefore owns the `guide` binding and forwards it over
  `lxb_shell_v1` (see below).

### The stick pointer

A console has a controller and no mouse, and plenty of what runs on one was
written for a mouse — a browser, an emulator's menu, a settings page. The tile
at the head of the guide's column turns the controller into one, inside the
application in front:

| Control                    | What it does |
| -------------------------- | ------------ |
| Right stick                | Moves the pointer |
| `A`, or R3                 | Left click |
| `B`, or L3                 | Right click |
| Left stick, or the D-pad   | Scrolls what is under it |

Two spellings for each button, because two hands reach for different ones: `A`
and `B` are where a hand goes without being taught, and the stick presses are
where a thumb already is while it is aiming. Nothing else is taken — the
shoulders still move between displays, and the left-hand face button is still
half of the keyboard's chord.

`A` can be a click at all only because it is not anything else at the time: the
shell drops every action but the guide button and the keyboard chord once an
application is in front of it, so `A` is doing nothing there. Back on the
shell's own screens it is Launch again, because there the pointer is switched
off.

The scrolling is continuous rather than a count of wheel notches, because a
stick is: pushed further to scroll faster, exactly as a touchpad is dragged
further. The D-pad counts as a stick pushed all the way over, so holding a
direction runs down a page rather than nudging it.

It is a choice made per application, not a mode the shell is in, because the
right stick is not free: in a game it is the camera. So it is remembered
against the application rather than the session, in
`~/.config/lxb/apps.toml`, keyed by the name the application gives itself:

```toml
[apps."org.mozilla.firefox"]
stick-pointer = true
```

The key is the `app_id` an `xdg_toplevel` sets, or an X11 window's class, which
the compositor reports over `lxb_shell_v1`. Not the window title, which is
a document name and changes under the setting. An application that sets neither
cannot be remembered at all — one entry shared by every nameless window on the
machine would be worse than none.

Which is also what the tile does with nothing in front of it to be about: it is
drawn, dimmed, and the highlight steps over it rather than stopping on a switch
that cannot be thrown. The mixer beside it is in that state permanently, until
there is a mixer behind it. Both are still drawn, because a line that came and
went with the application would slide the other tile across the sidebar every
time something was closed.

The pointer is the seat's own, not a drawing of the shell's: `wl_pointer`
belongs to the compositor, and the point is to reach the application. The shell
therefore reads the stick from `/dev/input`, as it reads the guide button, and
hands the movement over as `move_pointer` — after which it is an ordinary
pointer movement, constraints and relative-motion stream included, so a client
cannot tell it from a mouse.

The stick goes back to being a stick the moment one of the shell's own screens
is in front of the application — the guide menu, or a launch still waiting for
its window. There is nothing on either to point at, and the pointer stays
exactly where it was parked.

The on-screen keyboard is the one thing drawn over an application that does
*not* take the whole pointer away, because it does not need to: it is driven
with the D-pad, the left stick and `A` — the clicks and the scrolling — and has
no use at all for the right stick. So the two halves part company for as long
as it is up. Aiming carries on underneath the board, and the pointer is still
where it was left when the board goes away; `B` puts the board away, and hands
the buttons back with it.

One honest limitation, and it is the same one that makes the guide button work
at all: a controller is not routed through Wayland, so the application is
reading `/dev/input` too. Turning this on in something that uses the right
stick itself moves the pointer *and* the camera. It is meant for the
applications that ignore controllers entirely, which are exactly the ones that
need a mouse.

## The context menu

`A` does the one thing a row is for. Everything *else* that can be done to it
lives behind the top face button — `Y` on an Xbox pad, `Triangle` on a
PlayStation one — which is where a cross media bar has kept its options menu
since the first one. On a keyboard it is `Y` or `F10`, deliberately *not* the
`Menu` key: that one summons the guide, and with Steam running it is the only
thing that does so without Big Picture coming up alongside it.

It is a component rather than a screen. Whatever raises it hands over three
things: the rectangle of the control being acted on, a name for it, and the
rows. The panel then grows out of that rectangle, stands beside it on whichever
side of the display has room, and folds back into it when the menu is answered
or dismissed. Adding a command later is one line in a list; raising a menu
somewhere new is one function that returns those three things.

Three of them exist so far:

| Where | What it is about | Rows |
| ----- | ---------------- | ---- |
| The bar | The application on the focused tile, out of the disc it stands on | Information, Uninstall / Launch, Close |
| The bar | One of the user's own files, out of the same disc | Open, Open with, Delete / Sort, Cancel |
| The guide | The window under the selected card, out of that card | Move to next display, Move to previous display, Screenshot the app / Cancel |

A row can also lead to a *further* list rather than doing something — Open with
and Sort both do. The panel stays exactly where it is and swaps what is written
on it, once the row that asked has been seen going down; `B` steps back out to
the list it came from, and only closes the panel from the outermost one.

The rows themselves are ordinary furniture. A row can carry a glyph, sit in a
band of its own below a hairline, be warm for something there is no coming back
from, or be offered and out of reach — "Move to Other Screen" is drawn as an
outline on a session with one display rather than being left out, so the command
is still discoverable. The highlight steps straight over anything it cannot
stop on, wraps at both ends, and glides between rows rather than jumping. A list
longer than the display can hold scrolls under a panel that does not change
size, with an arrow at whichever end still has rows past it.

The panel is cut from the guide sidebar's glass — the same two pools of accent
light under one shallow refractive pane, and the same hairline over it — because
it is the same kind of object: a large quiet surface that things you can press
are laid on. The modal recipe the power dialog uses is deliberately not reached
for; that is a question about ending the session, and this is a note attached to
something you can still see behind it.

It takes every button while it is up. `B` closes it and goes back to whatever
raised it, one layer at a time; the guide button and the keyboard chord still
outrank it, and both put it away on their way past.

## The on-screen keyboard

A console has no keyboard, so the shell is one. It comes up by itself when an
application says a text field has the cursor, and can be summoned by hand at
any time — over an application that never said so, or over the bar with nothing
running at all.

| Control | Effect |
| ------- | ------ |
| D-pad / stick | Move between keys |
| `A` | Press the key under the cursor |
| `B` | Put the keyboard away — as does the key at its bottom right |
| Select + the left-hand face button | Show or hide it, anywhere |
| `Super+K`, `XF86Keyboard` | The same, from a keyboard |
| Any key on a real keyboard | Puts the board away, and is typed into the application |

The left-hand face button is `X` on an Xbox pad, Square on a PlayStation one
and `Y` on a Nintendo one — the same button in all three cases. Select is
likewise Back, View, Share, Create or `−` depending on whose pad it is, which
is why the corner hint draws both rather than naming either.

Select does nothing by itself: it is a modifier, and only a modifier. A
modifier that also opened the menu would make the chord two things at once —
the menu would appear behind the keyboard, and since the menu closes the board,
the face button after it could only ever be opening one afresh, so the chord
could show the keyboard but never hide it. The menu keeps the guide button,
`B` from the bar, and the compositor's own binding.

It rises through the display's bottom edge rather than fading in, and leaves
the same way, on the same quarter second. A keyboard that appeared on the spot
over a running application reads as the application having done something; one
that slides up reads as the shell putting it there — and one that vanished on
the keystroke dismissing it would read as the shell having lost it. What is
kept is a *position* rather than the moment it opened, so the two directions
are one movement: a board dismissed halfway through arriving falls back from
where it is, and one summoned again while it is still leaving comes up from
there. The keys go back to the application on the instant either way — a
quarter of a second unable to type would be the board's parting insult — so
what is still on screen during the fall is a picture and nothing more, and
clicks pass straight through it.

The guide is the one exception, and it does not slide: the menu is not this
screen with the keyboard taken off it but another screen, drawn over the whole
of the space the board would have been travelling through.

### What a real keyboard does to it

It dismisses it. The board takes the physical keyboard for as long as it is on
screen, and the first key pressed on that keyboard is the answer to the only
question the board was ever asking: nobody hunts for letters with a stick while
a keyboard is under their hands. So the board leaves, and the key goes on to
the application as though the matching key on the board had been pressed, with
whatever modifiers were held — Ctrl+C still copies, Backspace still deletes,
and the letter that dismissed the board is the first letter of the word. It
matters most where the board comes up on its own: a search box taking the
cursor should not cost the user a keystroke, nor leave them looking past a
keyboard they are not using.

Nothing on the board is navigable from a keyboard, deliberately. Arrows that
moved a cursor around a picture of the keys already under the user's hands
would be six keys spent on the one thing a keyboard makes pointless. Escape is
the one key that dismisses without typing, which is what it does on every
keyboard that appears by itself.

Having been dismissed that way, the board stops offering itself: the next text
field gets the corner hint rather than a keyboard, because the user has already
said once that they can type, and being told again at every text box is the
thing they dismissed. The controller shortcut still summons it — for the
evening the keyboard is put down and a pad picked up — and summoning it is what
forgets the refusal.

None of this is done by taking keyboard focus, which the board must never do —
focus is what carries text-input focus, so the moment the shell holds the keys
the application's field deactivates and the board puts itself away in the same
breath. `zwp_input_method_v2.grab_keyboard` is the input method's own way in:
the compositor routes key events to the shell instead of to the application,
and the field the board is typing into does not move at all. The grab lasts
exactly as long as the board does.

### The layout

The arrangement is ANSI, and complete: the function row from `Esc` to `F12`,
the number row starting at the backtick, Tab, Caps Lock, Enter, both Shifts,
the backslash where `\` belongs, Ctrl and Alt where the bottom row keeps them,
and an arrow cluster beside the space bar. The key widths are ANSI's own — Tab
1.5 keys, Caps 1.75, Enter 2.25, right Shift 2.75 — so the stagger down the
sides is the one printed on the keyboard the user already knows, and every row
comes to exactly the same width.

Two departures. The function row is drawn at a little over half height, which
is what a keyboard that has one looks like and what stops thirteen keys nobody
visits while typing a password from taking a sixth of the board. And the bottom
row keeps only the two modifiers that can be reached without holding anything
down, giving the space the rest would have taken to the arrows and the way out.

Every modifier latches, because a board driven one key at a time has no way to
hold anything: there is only ever one finger on it. Shift, Ctrl and Alt arm for
a single key and lock when pressed twice, so one capital costs one press and a
run of them costs two, and `Ctrl` then `C` is a copy. Caps Lock goes straight
to the lock, which is what it is for. A latched key stays lit after the cursor
leaves it, brighter when locked than when armed. There is no second page of
symbols: Shift reaches them, in the places they are printed.

Shift is the one modifier not sent as a modifier. The board's keymap gives
every capital a key of its own, so a shifted letter is reached by sending `A`
rather than `a` with Shift held — which also means nothing can be left latched
in the application if the board goes away mid-word. Ctrl and Alt have no such
key to send, so they go as the mask they are.

Five caps are drawings rather than words: the four arrows, because the shell
bundles Roboto precisely so it does not depend on what fonts a console has
installed and Roboto has no arrow glyphs; and the way out, because a keyboard
folding away is recognised across a room where a word has to be read. They live
with the other built-ins in
[`crates/lxb-desktop/src/glyphs/`](crates/lxb-desktop/src/glyphs/).

The keyboard is drawn over the application without taking its keyboard, and
that is not a nicety — it is the condition of the thing working at all.
Keyboard focus is what carries text-input focus, so a board that took the keys
would take them from the field it exists to type into: the application's text
input would deactivate and the board would put itself away in the same breath
it appeared. It is therefore driven from the controller, and lets the pointer
through to the application underneath.

### How it knows

Three standard protocols, none of them LineXinBar's own:

- `zwp_text_input_v3` is the application saying a field has the cursor.
- `zwp_input_method_v2` is the shell hearing about it. Its `activate` event is
  the only signal Wayland has that a keyboard should come up — nothing else in
  the protocol describes what is *inside* a window.
- `zwp_virtual_keyboard_v1` is the shell typing: it uploads a keymap of its own
  and sends keycodes through the seat, so the letters reach a terminal, a game
  and an X11 client as well as the field that asked for them.

The honest limitation, and it is every Wayland keyboard's: an application that
never sends `zwp_text_input_v3` has no way to say a field is focused, so the
board cannot come up by itself there. That covers Chromium without
`--enable-wayland-ime`, and every X11 client. It can still be summoned, and
typing into it still works — which is why the shortcut is unconditional rather
than a convenience: the applications that most need it are exactly the ones the
shell cannot tell have a text field, so it does not try to guess.

Summoned over the bar with nothing running, the board opens and types nowhere,
because there is no application to receive the keycodes. That is deliberate: a
shortcut that silently refuses on some screens is harder to learn than one that
visibly does nothing.

### The corner hint

When a field has the cursor, the guide menu is closed and the keyboard is not
up — which is what you get after dismissing it — a chip appears in the bottom
right of the display, over the application, naming the chord that brings it
back. It goes when the cursor leaves the field. Unlike the board itself, the
hint needs an application: it points at a text field, and a text field belongs
to one.

It names the two buttons by drawing them rather than by lettering them.
"Press X" is wrong on a PlayStation pad, which has no X, and worse than wrong
on a Nintendo one, where X is the button *above* the one meant — the letters
are swapped between the two commonest layouts. "Press Select" is no better; the
same button has carried five different names. A picture of the pad with one
button filled in is true on all of them. Both drawings are in the tree, at
[`crates/lxb-desktop/src/glyphs/`](crates/lxb-desktop/src/glyphs/), and
compiled into the binary the way the font and the shaders are.

## `lxb_shell_v1`

Layer-shell says nothing about either half of the problem above, so
[`crates/lxb-protocol`](crates/lxb-protocol) defines a small private
protocol generated from one XML file for both sides:

| | |
| ------------------- | ------ |
| event `guide`       | The compositor's guide binding fired. |
| event `keyboard`    | Its on-screen keyboard binding fired. Nothing else about typing goes over this interface — that is text-input, input-method and virtual-keyboard, all standard. |
| event `output_foreground` | Title of the topmost application on one display, empty when none. |
| event `foreground`  | The same for the session as a whole. Superseded; not sent from version 3. |
| request `close_output_foreground` | Ask that display's application to close. |
| request `close_foreground` | The same for the session as a whole. Superseded. |
| request `quit`      | End the session. |
| request `set_launch_output` | Name the display new applications should open on. |
| request `move_pointer` | Move the seat's pointer, as a mouse would. |
| request `pointer_button` | Press or release one of its buttons. |
| request `scroll_pointer` | Scroll where it is, as a wheel or a touchpad would. |
| event `output_app_id` | What the application on one display *is*, as opposed to what its title says. |
| request `set_output_hdr` | Drive one display in high dynamic range, and say how bright and how saturated. |
| event `output_hdr` | Whether a display can be driven in HDR, whether it is, and the peak it reports. |
| event `output_hdr_controls` | Which of the HDR settings that display can actually honour. |
| event `output_mode` | One mode a display can be driven at, with whether it is the current one and whether the display names it as its own. A batch of them ends in `output_modes_done`. |
| request `set_output_mode` | Drive one display at a different resolution and refresh rate. |
| event `output_transform` | Which way up a display's picture is drawn — and, by being sent at all, that this compositor is the one turning it. |
| request `set_output_transform` | Turn one display's picture, for a screen standing on its side. |
| request `hide_pointer` | Take the cursor off screen, because the user has picked up the controller. |

`output_foreground` is what lets the menu say *Close KWrite* and notice when an
application it started has exited. It is reported per display because the
overlay belongs to one display: labelling it from the session's topmost window
offers to resume, or close, something on a screen the user is not looking at.

A window is attributed to the display it covers most of, rather than every
display it touches — otherwise one spilling over an edge claims both.

`set_output_hdr` is a request rather than something the shell does itself
because neither half of HDR is a client's to touch: the metadata infoframe is a
connector property and the colour pipeline belongs to the CRTC. What the shell
owns is the decision. `output_hdr` comes back saying what actually happened,
which is what the Settings page reports — a shell that showed the switch as
taking effect when the connector refused would be the one setting in the shell
nobody could trust.

`output_hdr_controls` is separate from `output_hdr` because an event's
arguments cannot change once it has shipped, and it answers a different
question: not whether a display can do HDR, but which of the settings do
anything on it. It is what lets the page tell a control that is off from one
that has nothing behind it.

`set_output_mode` is a request for the same reason `set_output_hdr` is. A
connector's mode list comes off the hardware, changing it is an atomic commit
on a CRTC no client may touch, and everything on that display — every window,
every layer surface, the shell's own bar — has to be reconfigured around the
new size afterwards. `output_mode` carries the list rather than leaving the
shell to read it off `wl_output`, whose non-current modes are deprecated, and
it is what the page marks: a mode the hardware refused must not read as chosen.
A nested session reports no modes at all, because the size of its window is the
parent compositor's business and not this one's to change.

`set_output_transform` is a request for a different reason: turning is not
something the connector does at all. The picture is composited turned and
scanned out at the mode's own pixels, so no hardware can refuse it — but a
quarter turn swaps the display's logical width and height, which moves the
displays laid out beside it, re-arranges every layer surface anchored to it and
re-tiles every window on it. That is the compositor's, all of it.

`output_transform` is sent only for the displays this compositor turns itself,
which is what a shell needs and what `wl_output.geometry`'s own transform does
not answer: that one describes what a client should do about the output it is
drawing on, and it is advertised even for a display whose orientation belongs
to somebody else — a nested session, or a backend whose output carries a flip
of its own to compensate for the way it draws. A display absent from these
events is one the Orientation page leaves out rather than offers and cannot
honour.

`set_launch_output` exists because keyboard focus is the wrong thing to infer
the launch display from. Starting a second application from the guide hands
focus back to the *first* one long before the new window maps, so the new
window would join the old one's display. A shell drawing one surface per
display already knows the answer, so it says it outright — which also avoids
racing the round trip a focus change costs. The compositor treats it as a
preference and falls back to focus, then the pointer, then any output.

It is session-private and on the same trust footing as wlr-layer-shell: any
client of this compositor may bind it. The shell degrades cleanly without it —
on another compositor it falls back to signalling the process group of what it
started itself, and `Quit` simply exits the shell.

## Mouse and touch

The shell is drawn for a controller and none of that changes to be pointed at.
What a click does is what moving the selection there and pressing `A` does,
carried out through the same actions, so there is one answer to what every
control means rather than two that can drift apart.

One rule shapes the rest of it:

> Things that **stand still** light up under the pointer and answer the first
> click. Things that **move when they are selected** take one click to select
> and another to act.

The guide's sidebar, the context menu, the modal panel's buttons, the power
question and the on-screen keyboard are the first kind: hovering a row *is*
selecting it, exactly as it always has been on the board. The start screen's
own rows and the overview's cards are the second, and the reason is the shape
of the bar: selecting a row slides it to the middle of the screen, so a hover
that selected would pull whatever was under the cursor away and leave its
neighbour there to be selected in turn. The cursor would walk the bar across
the display with nobody touching the mouse.

| Input                    | What it does                                     |
| ------------------------ | ------------------------------------------------ |
| Left button              | Presses what is under it, or selects it first     |
| Right button             | Opens the context menu on what is under it        |
| Side button (rear)       | Opens the guide overlay, from inside anything     |
| Wheel                    | Moves the selection: the column, the rows of a menu, the deck of cards |
| Moving onto a display    | Hands control to that display                     |

The quick-settings bars and the mixer's rows are the one place a press carries
something with it: on the groove it sets the value where it was clicked, and on
the speaker at the groove's head it silences — which is what pressing the bar
has always meant.

Touch is the same hit test without the hovering. A finger going down selects,
lifting it presses, and a finger that has travelled more than a couple of dozen
pixels has stopped being a tap and presses nothing however it ends.

The pointer only reaches what the shell is actually showing. The input region
is cut to the same answer the drawing is, so a keyboard drawn over a game takes
clicks on its keys and nowhere else, and a launch splash takes none at all.

## The cursor

There is no cursor on screen until something moves one. A console is driven
with a controller, and an arrow parked in the middle of a start screen is a
thing the user cannot move and did not ask for, so the session comes up without
one and a machine with no mouse plugged into it never grows one.

| What the user does | The cursor |
| ------------------ | ---------- |
| Moves a mouse or a touchpad | Appears, where the movement puts it |
| Aims with [the stick pointer](#the-stick-pointer) | The same: that stick *is* a mouse |
| Presses a key on a keyboard | Goes away |
| Presses anything on a controller | Goes away |

Movement is the only thing that brings it back, and that is the whole of the
rule. A cursor that reappeared because a button was pressed would appear
wherever it happened to have been left, which is nowhere the user is looking;
moving it is the one gesture that also says *where*.

The keyboard half is the compositor's own — it sees those keys. The controller
half cannot be, because a controller is not a seat device: the shell reads the
pad straight from `/dev/input`, which is the only reason the guide button works
while a game holds the keyboard, and it leaves nothing for the compositor to
notice. So the shell says so, with `hide_pointer`. It says it for the presses
it acts on itself and not for the ones the stick pointer is clicking with —
with the stick pointer on, the whole pad is the mouse, and a cursor that
blinked out at every click would be one the user could not use.

Typing on the on-screen keyboard does not hide it either, for the same reason
in reverse: those keys arrive as a virtual keyboard rather than as a seat one,
and the hand pressing them may well be on the mouse.

## Keyboard focus

The shell asks for `Exclusive` keyboard interactivity whenever nothing is in
front of it, so the launcher is immediately controllable, including when it is
nested. With an application running it drops to `OnDemand`, and the compositor
then resolves focus in this order:

1. a layer surface demanding `Exclusive`,
2. otherwise the topmost application window,
3. otherwise any layer surface willing to take input.

That last step is what returns focus to the shell when the application exits.
Controller actions are paused whenever keyboard focus leaves the shell, so the
bar cannot react behind a running game. Pass `--grab-keyboard` to retain the
exclusive grab and controller input at all times instead.

Applications launched by the shell are explicitly given the same named
Wayland socket as the shell. It accepts an X11 display only through LineXinBar's
`LXB_XWAYLAND_DISPLAY` marker; arbitrary host `WAYLAND_SOCKET` and
`DISPLAY` values are discarded. The nested helper also starts a private D-Bus
session, preventing ordinary D-Bus activation from forwarding a launch to an
outer-desktop process. Applications with their own profile-based remote IPC
can still reuse an existing instance while nested; test those with a separate
profile or from the intended dedicated LineXinBar login session, where no host
desktop instance is running.

The shell and private XWayland server are one session unit. If their XWM
connection is lost, LineXinBar exits instead of leaving the shell with a stale
`DISPLAY`; a production service manager can then restart the complete session.
Waiting for XWayland's display-ready signal has a five-second deadline and
falls back to a Wayland-only shell if the server is missing, broken, or never
becomes ready.

## What draws, and when

A frame callback is how a Wayland client is told to draw its next frame, and
LineXinBar hands them only to what is actually on screen. Every application is
tiled across its whole display, so a window with another one in front of it is
not partly visible — it is not visible at all, and it stops at no frames until
it comes back to the front. A video left playing behind a terminal decodes
nothing.

This is decided from the stack rather than from what shows through it. A
translucent application in front changes what the user can see of the window
behind, not whether that window is worth drawing, so it goes quiet either way.
Anything the front application only partly covers keeps drawing: a bar
reserving space at the edge of the display tiles the application into what is
left, and a window standing out past that has pixels of its own on screen.

The guide menu is the exception. Its overview shows every window on the
display at once, and those cards are the live windows rather than screenshots,
so opening it starts them all again and closing it puts them back to sleep.

Layer surfaces are never held back, covered or not. The shell is one, it
presents FIFO — which paces on exactly these callbacks — and it runs a single
thread, so a shell denied them blocks inside its own present instead of
idling, never hears the guide button, and takes the session with it. It
already stops drawing by itself once something covers it, so there was nothing
to reclaim there. Applications are different: one blocked in its present is
one costing nothing, and it wakes the moment it is back in front.

X11 applications are a partial exception in the other direction. Xwayland
absorbs the missing callbacks rather than passing the stall on, so an X11
client can carry on rendering into a window nobody can see; what is saved
there is LineXinBar's compositing, not the client's drawing. Wayland-native
applications stop properly.

## Configuration

`$XDG_CONFIG_HOME/lxb/config.toml` (usually
`~/.config/lxb/config.toml`). Every field is optional; see
[`docs/configuration.md`](docs/configuration.md) for the full reference and
[`examples/config.toml`](examples/config.toml) for a commented sample.

## Default keybindings

| Binding                | Action                          |
| ---------------------- | ------------------------------- |
| `Ctrl+Alt+Backspace`   | Quit the compositor             |
| `Super+Q`              | Close the focused window        |
| `Super` on its own, `Super+Home`, `XF86HomePage`, mouse side button | Show the guide overlay |
| `Super+K`, `XF86Keyboard` | Show the on-screen keyboard  |
| `Super+Tab`            | Cycle windows on this output    |
| `Super+←` / `Super+→`  | Focus the previous/next output  |
| `Super+Shift+→`        | Move the window to the next output |
| `Ctrl+Alt+F1`…`F12`    | Switch VT (udev backend only)   |

The Windows key is the home button, and it is watched for rather than looked up
in the table: a bare modifier is half of every chord in it, so a guide that
opened on the press would shadow all of them. What names the key on its own is
the release — down, up, and nothing in between — and both edges still reach the
client, because swallowing the release of a modifier whose press was forwarded
leaves an application holding a Super it is never told about again.

Any of these can be overridden in the `[keybindings]` table, except the guide:
its chords always summon the home menu, ahead of every other binding, and
`"<chord>" = "guide"` adds another one rather than moving it.

## Architecture

```
crates/lxb-compositor/
  state.rs        global state, split so a render pass can borrow the
                  backend and the compositor state at once; session shell
  handlers.rs     Wayland protocol handler implementations
  outputs.rs      multi-display layout: positions, scale, transform, tiling
  input.rs        input routing, focus policy, keybindings
  focus.rs        common Wayland/X11 keyboard, pointer and touch targets
  shell_control.rs  compositor half of lxb_shell_v1
  xwayland.rs     private XWayland server's X window manager and selections
  render.rs       render element assembly, shared by every backend
  backend/
    winit.rs      nested, one window
    x11.rs        nested, one window per virtual output
    udev.rs       DRM/KMS, libinput, libseat, multi-GPU

crates/lxb-protocol/
  protocols/      lxb-shell-v1.xml, the single source for both sides
  lib.rs          wayland-scanner bindings, client and server behind features

crates/lxb-desktop/
  apps.rs         .desktop parsing and Plasma-style categorisation
  icons.rs        icon theme lookup, PNG/SVG rasterisation
  model.rs        the shared catalogue, and one cursor per display
  guide.rs        the overlay's modes and menu
  menu.rs         the context menu: entries, selection, scrolling and
                  growth, with no idea what raised it
  media.rs        the walk over $HOME for music, films and photographs,
                  what order the rows are in, and what opens one
  trash.rs        the freedesktop trash, for the Delete row
  pointer.rs      the right stick as a mouse, and which applications it is
                  turned on for
  keyboard.rs     the on-screen keyboard: its keys, the input method and
                  virtual keyboard behind them, and the grab that lets a
                  real keyboard drive it
  system.rs       volume and brightness, off the main thread
  ui.rs           layout: model to quads and text runs
  gpu.rs          wgpu renderer, one atlas and two pipelines
  shaders.wgsl    animated backdrop, instanced quads
```

## Not implemented

Worth knowing before you rely on this:

- **A see-through overlay without a blendable surface.** The overlay asks for
  a premultiplied-alpha surface so the running application shows through it.
  A driver offering only `Opaque` gets a working menu on a solid background
  instead, and says so in the log.
- **Screen capture.** No `wlr-screencopy` or xdg-desktop-portal, so
  screenshots and screen sharing do not work from inside.
- **Pointer- or touch-driven shell navigation.** The XMB has no pointer,
  touch, or gesture navigation handlers yet. The compositor still routes those
  input types normally to other clients; keyboard and game-controller XMB
  navigation are fully supported. The stick pointer above is about the
  application in front, not about the shell.
- **The per-application volume mixer.** Its tile is drawn beside the stick
  pointer's and does nothing yet. The session's own volume works.
- **Unlimited relative-pointer capture in the nested debug backends.** They
  synthesize relative events from the parent cursor and enforce client locks,
  but movement stops at the outer window edge because Smithay's nested event
  adapters do not expose the parent's raw-motion stream. The native `udev`
  backend used for a dedicated LineXinBar/Steam Deck session receives true
  libinput relative motion and is not edge-limited.
- **Client colour management.** HDR is an *output* setting: the compositor
  drives the connector in BT.2020/PQ and re-encodes its own sRGB output to
  match. There is no `wp_color_management_v1`, so an application cannot hand
  over HDR content of its own — a game rendering in scRGB or PQ still submits
  an sRGB buffer and is displayed as SDR content on an HDR signal.
- **Variable refresh rate.** `adaptive_sync` is parsed from the config but not
  yet applied.

## License

GNU General Public License v3.0 only. See [LICENSE](LICENSE).
