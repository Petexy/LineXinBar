# LineXinBar

A micro Wayland compositor with first-class multi-display support, plus a
console-style lattice shell that runs inside it.

Two binaries:

| Binary        | What it is                                                    |
| ------------- | ------------------------------------------------------------- |
| `lxb`         | The compositor. DRM/KMS on a TTY, or nested inside a desktop.  |
| `lxb-desktop` | The shell: a console-style lattice launcher, on the GPU.       |

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
| `libasound`                                | The shell's effects and Start music         | shell      |
| `libavcodec` / `libavformat` / `libavutil` / `libswscale` | A wallpaper of the user's own: [Custom wallpaper](#custom-wallpaper) decodes their picture or plays their film | shell |
| `libpipewire-0.3`                          | The frames a shared screen is carried on    | portal     |

`libpipewire`'s and FFmpeg's Rust bindings are both generated at build time, so
building the portal or the shell also wants **`clang`** for `bindgen`.

FFmpeg is linked for pictures and films and for nothing else. A film used as a
wallpaper is silent, and cannot be anything else: its audio stream is never
opened and no audio decoder is ever made, the crate is built without
`software-resampling` and without `device`, and the only thing in this shell
that makes a noise at all is its own handful of embedded clips.

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

### Updates

Settings > Updates is a press and a bar: a check that ends in a count and an
**Update now**, a bar that fills as the machine's own package managers do the
work, and a word about a restart. The sources are listed explicitly under it
— the system, Flatpaks, AUR, Snaps and configured Nix/Guix profiles — and
what is waiting is a scrollable folder of the column. Update now is the one
confirmation: every tool runs unattended after it, the password for a root
step is asked by the shell's own polkit panel in the update panel's place,
and what a tool asks anyway — a conffile, a helper's sudo — is answered
with a Yes and a No or a field. **Full output** is the whole transcript in
an eighty-column terminal frame. Firmware updates use **fwupd**, now a
required desktop dependency alongside **polkit**, and exclude BIOS/UEFI and
unclassified devices from routine installation.

See [Updates](docs/updates.md) for provider coverage, administrator configuration,
recovery behavior and validation limits. Cross-distribution adapters are an
initial implementation; they still need testing on their target distributions.
Distributors building immutable systems can ship a
[custom System update provider](docs/updates.md#custom-system-update-providers);
the developer guide includes manifests and an adapter script example.

### Optional at runtime

Each of these is looked for when it is wanted, and its absence costs exactly
one feature:

| Program                                   | Gives                                  | Missing                                        |
| ----------------------------------------- | -------------------------------------- | ---------------------------------------------- |
| `Xwayland`                                | X11 applications                       | A Wayland-only session; logged, never fatal    |
| `dbus` (`dbus-update-activation-environment`) | D-Bus activation inside the session — including the desktop portal, which is what screen sharing is | No screen sharing at all, and activated apps may appear on the outer desktop |
| `wpctl` / `pactl` / `amixer`              | The volume bar and the volume keys, in that order of preference. `pactl` also lists what each application is playing, which is the mixer | No volume bar, and the volume keys do nothing; with `amixer` alone, a mixer holding only the session's own output |
| `ddcutil`                                 | Brightness for external monitors, over DDC/CI | Brightness only where the kernel has a backlight |
| `xdg-open`                                | Opening one of the user's own files when nothing installed declares its type | Those rows are listed but report that nothing opens them |
| `ffmpegthumbnailer` **or** `ffmpeg`       | A frame of each film, on its row in Video | Films keep the film-strip glyph; photographs are unaffected |
| `pipewire`                                | The frames a shared screen is carried on | `lxb-portal` will not start, and screen sharing is unavailable |
| `xdg-desktop-portal`                      | The front desk applications ask for a screen or for a file — [screen sharing](#screen-sharing) and [choosing a file](#choosing-a-file) both need this and `lxb-portal` | Applications find no portal: none can share a screen, and each falls back to whatever file dialog it has of its own |
| `steam` (native or Flatpak)               | Playing and installing anything in the Steam column | The account still signs in and the library is still listed, but nothing in it starts or downloads: every row says so rather than doing nothing |
| `lxb-retroarch` (a package of its own)    | [RetroArch and your own console games](#retroarch-and-your-own-console-games): a row under Steam, and a column of the consoles in your ROM folder | The shell never mentions RetroArch at all — no row, no column, no page under Settings |
| `flatpak`, with `lxb-retroarch` installed | Installing RetroArch from the shell, and running the Flathub build | The row says RetroArch is not installed and that there is no flatpak to install it with; a distribution package of `retroarch` is used in preference either way |

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
- **`/dev/uinput`**, to keep [the guide button](#the-guide-button-is-the-shells-alone)
  off every controller an application can read, and to build the gamepad the
  kernel gives the second-generation Steam Controller no driver for. Without it
  every pad the kernel drives still works exactly as it did, with the guide
  button reaching applications as well as the shell — and that one pad works in
  the shell but in nothing the shell launches.
- **That pad's `hidraw`**, for the same reason and from the other end: nothing
  drives it, so the shell reads its report directly. `hidraw` nodes are
  `0600 root:root` and systemd's own rules tag none of them.

  Both are granted by `packaging/files/70-linexinbar-input.rules`, which the
  desktop package installs: `uaccess` on each, so they belong to whoever holds
  the active session on the seat rather than to a group every account could be
  added to. A checkout being run without installing the package wants that file
  in `/etc/udev/rules.d/`, or these two are the parts of the controller work
  that quietly do not happen. What that grants is wider than it sounds, and is
  set out below.

### What the input rule actually opens

`uaccess` narrows *which account*. It says nothing about which program. The ACL
names a uid, and every process running as that uid is inside it — the shell, and
equally Valve's client, every game it starts, whatever the software hub
installed, a browser. That is the same list this project elsewhere calls
software nobody here wrote or can vet, and the reason
[`lxb_shell_v1`](#lxb_shell_v1) is refused to all of it. These two nodes are the
place where that rule does not hold, and the size of the exception is worth
saying out loud rather than leaving to be inferred from a tag name.

- **`/dev/uinput` is input injection.** Anything running as the user can create
  a virtual keyboard, pointer or gamepad and synthesise events on it. Those
  events reach the compositor through evdev and libinput exactly as the real
  keyboard's do, because by then there is nothing left to tell apart — which is
  Wayland's usual guarantee that one client cannot type into another, gone. A
  game does not need `lxb_shell_v1` to close a window if it can press the keys
  that close it.
- **The pad's `hidraw` is granted read *and* write**, though the shell only ever
  reads: it opens the node read-only, and
  [`steam_hid`](crates/lxb-desktop/src/steam_hid.rs) sets out at length why
  writing to that pad — lizard mode, rumble, the gyro, the lights — is Steam's
  business and not this shell's. The write bit is not a choice made here.
  systemd's `uaccess` builtin grants `rw` and takes no argument saying
  otherwise, so the ACL carries it whatever `MODE=` says. Anything running as
  the user can send that controller feature reports.

Neither closes by moving the work somewhere more trusted, which is the first
thing that suggests itself. The compositor runs as the same user, and takes its
devices through libseat, which asks logind, which hands out only what is
assigned to the seat — `/dev/uinput` is `misc` with a static node and is on no
seat at all. Closing it needs a component the games are not: something running
as root that opens these nodes and passes the descriptor to a process it has
authenticated, exposing the few operations the shell actually performs rather
than the node. That does not exist here. Until it does, a LineXinBar session
grants every program the user runs the ability to inject input, and the honest
description of the boundary is that it is drawn around the account and not
around the shell.

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
pacman -S --needed rust pkgconf clang wayland libinput seatd systemd-libs libdrm \
    mesa libxkbcommon libglvnd libx11 libxcb libxcursor libxi libxkbcommon-x11 \
    alsa-lib ffmpeg
pacman -S --needed xorg-xwayland dbus wireplumber ddcutil   # optional
```

Debian and Ubuntu:

```sh
apt install build-essential pkg-config cargo clang libwayland-dev libinput-dev \
    libseat-dev libudev-dev libdrm-dev libgbm-dev libxkbcommon-dev \
    libegl1-mesa-dev libx11-dev libxcb1-dev libxcursor-dev libxi-dev \
    libxkbcommon-x11-dev libasound2-dev libavcodec-dev libavformat-dev \
    libavutil-dev libswscale-dev
```

Fedora:

```sh
dnf install cargo pkgconf-pkg-config clang wayland-devel libinput-devel \
    libseat-devel systemd-devel libdrm-devel mesa-libgbm-devel \
    libxkbcommon-devel mesa-libEGL-devel libX11-devel libxcb-devel \
    libXcursor-devel libXi-devel libxkbcommon-x11-devel alsa-lib-devel \
    ffmpeg-free-devel
```

Package names drift; the library list above is the thing to match if yours
disagrees. The shell also wants a Vulkan driver at runtime where one exists
(`vulkan-radeon`, `vulkan-intel`, `mesa-vulkan-drivers`), and falls back to GL
where it does not.

Everything built here reports one version, and it is the single line in
[`VERSION`](VERSION) at the root of the checkout: every package definition
reads that file, and the compositor and the shell both refuse to build against
a manifest that has drifted away from it. `./scripts/bump-version.sh 0.2.0`
moves it, along with the two places that have to carry the number as a literal;
[`packaging/README.md`](packaging/README.md) says which and why.

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
settings the way a console's Settings region holds its own. Everything after it
comes from `.desktop` files in the usual XDG search path, grouped into the
categories Plasma's launcher uses: System, Multimedia, Graphics, Internet,
Office, Games, Software, Development, Education & Science, Utilities, Waydroid
and Other. A category with no application in it is hidden — except Settings,
which is part of the bar rather than a result of what is installed.

Two of those columns are not filled by a main category, because no main
category answers what they are about. **Software** holds the stores and
software hubs — anything declaring `PackageManager` alongside `System` or
`Settings`, plus a short list of the well-known stores that declare neither,
Plasma's Discover among them. **Waydroid** holds the Android applications this
machine can run: Waydroid writes `X-WayDroid-App` on every entry it generates
and no main category at all, so before that column they fell through to Other.
Waydroid's own launcher is filed with them, at the head of what it runs. An
entry Waydroid marked `NoDisplay` stays hidden, as it does everywhere else in
the shell.

Which column a session opens on is **Settings > System > Startup category**,
and it is Games unless somebody has said otherwise. The page is the bar itself —
one row per column this machine has, each wearing its own mark — and a column
named by the setting but not on the bar this session, a Steam library nobody is
signed in to, is offered all the same and says why it is not there. Where the
named column is genuinely absent the shell opens where it always did: the first
column with something in it.

An application can also ask for its icon to be drawn in the shell's own
material rather than as the picture its theme holds, by naming `lxb` in its
`Keywords` or `Categories`. The icon is then measured into a distance field and
the quad shader cuts a bead of water to it, exactly as it does for the shell's
own marks, so the row wears the same material as the column heading above it.
It is opt-in because what the shader is handed is the *silhouette*: a drawing
made to be a shape comes out as one, and a photograph comes out as a rounded
slab of glass.

Multimedia carries two subcategories of its own, Music and Video, Graphics
carries one, Images, and System carries **Files**. What is in all four is the
user's own files rather than applications — the first three by kind, gathered
from everywhere under `$HOME`, and the fourth by where they actually are. No `.desktop` file can say which half of Multimedia an
application belongs to — the menu spec requires `AudioVideo` alongside `Audio`
or `Video` but never the reverse, so an entry may declare `AudioVideo` and
stop, and many of the best-known media applications do exactly that — so the
players and the editors stay in their columns, where nothing has to be guessed
about them.

### What the buttons do

The start screen writes its own controls in the corner opposite the clock:

```
                                  Select  (A)   Options  (Y)   Guide  (⌂)
```

**Drawn rather than lettered.** "Press A" is wrong on a PlayStation pad, which
has no A, and worse than wrong on a Nintendo one, where A is the button on the
opposite side of the cluster from where an Xbox layout puts it. So each is a
picture of the cluster with the button in question filled in, which is true on
all of them — the same drawings the file panel's own legend uses, laid out by
the same code, because it is the same promise: somebody who has learned that the
filled bead at the bottom of the cluster takes a row must not have to learn it
again in a file dialog.

**And named by whichever control is in hand.** The same act is South on a pad
and Enter on a keyboard, and there is no wording that covers both without naming
neither, so the shell says the one the user's hands are actually on — it watches
for a button, a stick or a key and remembers which came last. On a keyboard the
row reads Enter, the right mouse button and Super. Options leaves the keyboard
deliberately: no key printed on a keyboard says "menu" to as many people as the
right button does. Super is the one key with a mark for it that is not somebody's
logo, and it is the binding the compositor holds back from every application so
that the guide is always reachable.

**Options comes and goes with the row.** Most of the bar is objects — an
application, a song, a file, a game — and every one of them has a context menu;
a settings value and a subcategory are not objects and have none. The legend
offers the button exactly when a press on it would raise something, because it
asks the very function the press does. A legend naming a button that does
nothing is worse than naming none.

**Guide never does.** It is the one press that works from everywhere in the
session, an application holding the whole screen included, and a legend that
dropped it on some rows would be hiding the way out.

It is drawn on the display being driven and no other, and it gives the corner up
to anything with buttons of its own: the guide overlay, a context menu, the
centred panel, a file being carried, an application's file question — which
draws a legend of its own — and the on-screen keyboard's corner chip, which
stands in this very corner. Two clusters of button pictures in one corner is not
a legend, it is a pile.

**It can be turned off** at `Settings > System > Button hints`, and it is on
until it is. Somebody who does not need it is exactly the person who will find
the switch; somebody who does will never go looking for a setting to reveal what
they do not know is missing.

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

Five minutes is the right interval for a collection nobody is looking at, and
the wrong one in the two cases where somebody is. So neither of those waits:

- **A file this session wrote is shelved as it is written.** The shell asked
  for the screenshot and knows the path in the answer, so that one file goes
  straight onto the shelf — a picture taken while standing in Images appears in
  the column a moment later, under its own thumbnail, without anything being
  searched for.
- **Stepping into a shelf brings the next walk forward.** Opening Music, Video
  or Images is somebody asking what they have, and the answer should include
  the film they recorded a minute ago in an application this shell knows
  nothing about. It is the same walk started early rather than a scan of its
  own, and it cannot happen more than once every twenty seconds, so a column
  stepped in and out of ten times still costs one pass. Walking *past* the row
  asks nothing: idle scrolling must not read the disk.

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
merge, the sort, the search, letting go of the list that was replaced. The
shell sends four things the other way — list this shelf differently, show only
what matches this, this file has been deleted, and here are the rows you can
let go of now — and holds no files at all.

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

#### Searching a shelf

Each of the three columns carries a field at its head, and pressing `A` on it
raises the on-screen keyboard, because on a console there is nothing else to
type with. What is typed goes onto the row itself rather than into a panel over
it: the row *is* the field, and it carries the caret while the board is up.

A column of music still opens on music. The field stands over the list where
anything standing over a list stands, and is reached by pressing Up from the
top of it — the one direction nothing else was using, and where a person looks
for the thing above the first thing. Opening on it instead would make every
visit to a shelf begin by stepping over a control nobody asked for. The same
goes for a shelf that has just been reordered: what "newest first" asks to be
shown is the newest file, not the search.

Walking off the field ends the typing, and the board goes with it. That is
asked once a frame rather than of any particular way of leaving, because the
bar can be walked out from under an open keyboard by routes that never touch
the field — a pointer resting on the category row is one, and control passing
to another display is another.

The column narrows as each letter lands. A file matches when what was typed
appears anywhere in its name, ignoring case, so `radio` finds both *Radiohead*
and *Old Radio Show*. The folder is not searched — an album answering to the
name of the shelf it sits in would bury the one track that was meant — and
neither is anything on another shelf: a search of Music returns audio files
because the shelf it narrows holds nothing else. The line under the field says
how much of the collection is being kept back (`2590 of 517488 images match`),
and the row that the column hangs on says it too, so a narrowed column can be
told from a small one without stepping into it.

`Enter` puts the board away and takes the cursor to the first match. Back does
the same without moving it. Neither undoes the search: the column has been
narrowing in plain sight with every letter, so there is no earlier list still
on screen for a cancel to mean — what puts the whole shelf back is the **Clear
search** row, which is on the column for exactly as long as there is something
to clear. A search lasts the session and is not written to the settings file:
an order is a preference, and a search is something somebody is doing.

The narrowing itself happens where the files are, on the same worker and for
the same reason as the sort. Half a million photographs are filtered per
keystroke there rather than between two frames, and the field does not wait for
it — the letter is on the bar on the frame the key was pressed, and the rows
under it are whatever the worker last finished. So the two are never
inconsistent with each other, only with the future.

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

`Y` on one of these rows raises a menu of six, in two bands — four that act on
the file and two that do not:

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

**Rename** opens a field on the row and is described [with the folder
listings](#changing-a-name), where it does the same thing to the same effect.

Copy and Move are **not on this menu**, and are missing rather than greyed —
the one place in the shell where a row that exists elsewhere is simply not
drawn. A shelf is a library rather than a folder: it gathers one kind of file
from everywhere the user keeps them, so there is no column for the picker to
open in and none for the file to appear in when it lands. Greyed is the shell
saying "not at this moment", and it is owed the user an idea of what would make
it the right moment — install something that opens this type, or look at a file
that is your own. There is nothing of the kind here: the answer would be "go
and find this song in Files", which is a different column about a different
thing, and two dead rows on every song, film and photograph in the machine is a
high price for a panel that keeps one shape.

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

### Files

System's first row opens the disk itself. Where Music, Video and Images answer
"what have I got", this answers the other question — "what is *in there*" — and
it is the same tree of columns that answers it: a folder is a subcategory, a
file is a row, and the trail of columns behind the cursor is the path.

```
System  >  Files  >  Root  >  usr  >  lib
```

It opens on three kinds of place. **Home** is the user's own folder; **Root**
is `/`; and after them comes a row per drive that is mounted. Each says how
much room is left on it. The list is built on the press that opens it rather
than when the shell starts, so a stick plugged in half an hour into a session
is on it the moment somebody goes looking.

A drive is a mounted filesystem backed by a device node, or a network share.
`/` is not one of them, because it is the Root row; nor is any of the mount
points a machine makes for itself (`/boot`, `/home`, `/usr`, `/var` and the
rest), which on a machine with subvolumes would otherwise offer the same disk
five times under the names of its own directories. Loop devices count only
under `/run/media`, `/media` and `/mnt`, where they are an `.iso` somebody
attached rather than a packaged application the system mounted.

Inside a folder the subcategories come first, then the files. A folder says
when it was last written until it has been opened, and afterwards what was
found in it (`239 folders, 5804 files`); a file says how big it is and when it
was written. Files of a kind the shelves already know keep that shelf's mark —
a song is drawn as a song here too — an archive is drawn as a carton with its
lid on, and everything else is a page. A mark is for a kind the shell can do
something with: there is a shelf for a song and an
[Extract](#getting-what-is-inside-an-archive) for a `.tar.gz`, and what a `.pdf`
is, is written on the row in words. Dotfiles
are left out: nobody's photographs are in `~/.cache`, and the shell's own
settings are a column of their own.

Every column here carries the same field at its head that the three shelves do,
and it means the same thing: `A` on it raises the on-screen keyboard, what is
typed goes onto the row itself, and the column narrows as each letter lands. A
name matches on any part of it, ignoring case. The row above goes on saying
what is in the folder while the field says what is being shown of it — `4 of 6
items match` — and the **Clear search** row under it is there for exactly as
long as there is something to clear.

The field is never what a column opens on. It stands over the list where
anything standing over a list stands, and stepping into Files puts the cursor
on Home, one row below it — the same arrangement a column of music has, and for
the same reason: opening on the field would make every visit begin by stepping
over a control nobody asked for. Pressing Up from the first row is how it is
reached.

A search belongs to the looking somebody is doing rather than to the folder.
Stepping out and back in is a fresh visit and shows everything that is there,
because a column that arrived already narrowed by what was typed in the last
one would be hiding files with no field in sight to say so.

Narrowing a folder is *cheaper* than opening one. A shelf is half a million
files held on a worker, so a search there is a message and the rows arrive when
they are ready; a folder is one directory, so the search is one more `readdir`
— and only the rows the query keeps cost the `stat` behind a size and a date.

**Sort** is on the menu over any file, and it offers the same nine orders the
shelves do: name either way, size either way, type, created either way and
modified either way. It is about the column rather than the file, as it is on a
shelf — and about every folder rather than this one, because an order is how
somebody reads a list and a shell that had to be told again in each directory
would be asking them to say the same thing over and over. It is remembered in
`[media-sort]` of the settings file under `Files`, beside the three shelves'
own orders.

Folders stay above files in all nine. Two of them a directory cannot answer
for — it has no type, and what `stat` gives for its size is the size of the
index rather than of what is in it — so in those the folders keep the
alphabetical order somebody can find them in, and only the files are ranked.

A folder is read on the press that opens it, on the thread that draws, and
never on the way past — a cursor walking down `/usr` passes a hundred
directories on its way to one. An ordinary folder takes under a millisecond
and the largest on a stock machine, `/usr/lib` with six thousand entries,
takes sixteen: one frame, once, on a press somebody made. Nothing is cached, so
what is on the bar is what was on the disk when it was pressed, and the folders
beside the one being opened give up their rows as it opens — the tree holds the
path the user is standing in and not everywhere they have been. A listing stops
at ten thousand rows and the row above says how many were left out.

`Y` over a file offers the shelves' menu with two more rows in it — Open, Open
with, Delete, Copy, Move, Rename, Sort, Cancel — and every row it shares means
here what it means there. **Copy and Move are the two, and this column is the
only place they appear at all**: a row here stands in a folder rather than in a
library, so there is somewhere for the picker to open and somewhere for the file
to appear when it lands. Delete is greyed
outside the user's home directory, which in this column is most of what can be reached — the
machine's own files can be looked at from here and never destroyed from here.
Copy and Move are not greyed with it: taking a copy of one of the machine's own
files is not destroying it, and whether the folder chosen will have it is the
filesystem's answer to give at the moment the transfer runs rather than the
menu's to guess.

`Y` over a **folder** offers a shorter list — Copy, Move, Rename, Sort, Cancel — and
only inside a listing: Home, Root and the drives are rows of the same kind
carrying the same kind of path, and none of them is a folder anybody may pick
up. There is no Open, because a folder is not opened by a program and pressing
it already opens the column; and no Delete, because a folder is however many
files deep and "do you want to delete *this*?" cannot honestly be asked about a
name standing for a thousand things nobody can see. Where both rows lead is
[the folder picker](#carrying-a-file-somewhere-else).
Opening a file starts it in whatever the desktop already opens that type with,
exactly as pressing it on a shelf does, and the loading screen carries the
file's own name.

A photograph or a film in a folder is drawn as itself, in the round hole its
mark would have had. Not as a card: a shelf of photographs is a column of cards
because everything on one *is* a picture, and a folder holds folders, documents
and photographs together — a column that was half cards and half rows would be
answering two shapes at once. So the rows stay rows, and the picture takes the
place of the mark of its kind.

The hole is square and a photograph almost never is, so the middle of it is
shown. Fitting the whole picture inside the circle would leave two empty
crescents round it, which down a column reads as pictures of several different
sizes; stretching it to fill would give the wrong face. The middle is what
every gallery on every phone shows in a grid, and it is where the subject of a
photograph nearly always is.

They are made the same way the shelves' are and out of the same cache — the
rows within four of each display's cursor, two workers, nothing ahead of time —
so a folder of six thousand files costs six thumbnails.

#### Carrying a file somewhere else

Copy and Move are the first two rows of that menu that need a *second* place
named before anything can happen, and where they lead is the same bar again,
mirrored.

The screen it opens is one row and one column. The thing being carried stays
where it was picked, alone, with the rest of the start screen taken away from
around it — everything but the corner's clock, which is part of the wallpaper
more than it is part of the controls. Beside it, in from the right, comes a
column of the folder it is in now, with **Paste** standing at the head of it
where a shelf's search field stands. What is under the head row is the folder
itself: the folders in it, which can be walked into, and the files, drawn
quieter — they are there so the folder is recognisable, and there is nothing
behind a press of one, because nothing can be filed inside a file.

**A column never opens on Paste.** It opens on the row below it, whatever that
row is, and the file being carried is one press of Up away from being filed.
Paste is the one row on this screen that acts: a column that opened on it would
put the end of the journey under the user's thumb at every step of the journey —
walk into a folder to see what is in it, press `A` out of habit, and the file
has been filed somewhere nobody chose. Reaching for something is what says it
was meant. A folder with nothing in it has only the one row, and there the
selection has nowhere else to be.

```
                     osu.appimage          Paste  ·  Copy here      AppImages
                                           osu.appimage
                                           r2modman.appimage
```

It is driven the way the bar is driven, with one thing the other way round:
the columns of the path stand to the **right** of the one being stood in, so
**Right is the way back out** and Left is the way in. Two lists of folders both
walking left would be the same gesture meaning two different things on one
screen; mirrored, the hand knows which of the two it is driving without being
told. Right goes back as far as `/` and no further — the picker is a path, and
every path on this machine ends there. There is no Volumes row at the top of
it: a mounted drive is under `/` and is reached by walking down to it.

Pressing Paste ends the journey. `B`, `Escape` and the right mouse button give
up on it, wherever on the display the click lands — the whole screen belongs to
the transfer while it is up, so there is nowhere on it a right button could be
asking about something else.

Paste says under itself what pressing it would do — *Copy here*, *Move here* —
and where it cannot be pressed it says that instead, greyed: **It is already
here** for a move into the folder the file is already in, and **It cannot be
put inside itself** for a folder being carried into itself or into anything
under it, which would copy until the disk was full.

A name already taken in the chosen folder stops the transfer and puts a
question up: **Keep both**, **Replace**, **Cancel**, in that order and opening
on the first, because it is the only one of the three that cannot lose
anything. Keep both writes `osu (2).appimage` — the number before the
extension, where every desktop this sits beside puts it. Replace is drawn warm
and in the shell's fixed red, and it is a promise about the *name*: a folder
written over a folder is merged into it rather than put in its place, because
deleting the hundred files already in there is not something the panel
mentioned and not something a press can be taken to have asked for. A copy into
the folder the file is already in is not a clash at all — it is a duplicate,
which is a thing people ask for, and it lands beside itself under a free name
with nothing asked.

The copy runs on a thread, because a file is as big as it is. Nothing is shown
for one the disk finishes inside a couple of frames; past a third of a second a
panel says what is being carried, and it stays until the transfer ends. Ending
says nothing and shows nothing — the user watched themselves choose a folder,
and a panel telling them it worked would be a button to press to get back to
the screen they were already on. What they get instead is the column, read
again, with the file in it or gone from it. A failure *is* a panel, carrying
what the filesystem actually said, because a command that silently either
worked or did not is a command nobody trusts twice.

#### Changing a name

**Rename** is on the same menu, and on the shelves' as well — the one of these
three rows that is: a song has a name wherever it is being looked at from,
whereas Copy and Move need a folder to have been opened. It opens a field on the row itself rather
than a panel over it, which is the same thing pressing a search field does —
what a press means there is "I am about to type", and the answer to that is a
keyboard.

What it opens with is what the row was showing. In a folder that is the file
name and all of it; on a shelf it is the title without the extension, because
that is what the row says — nobody thinks of a song as `Yesterday.flac`, and a
field that opened with an extension the user had never been shown would be
asking them to look after something the shell had been hiding. The `.flac` goes
back on the end of whatever they write.

`B`, `Escape` and the right mouse button put the old name back and give up. This
is the one thing a search field does *not* do, and the difference is what the
two are: a search has been narrowing the column with every letter, so there is
no earlier list left to return to and Escape does not pretend otherwise; a name
has changed nothing at all until it is accepted, so giving up costs nothing and
is therefore free. Nothing reaches the disk until Return.

Three names are refused before the filesystem is asked — nothing, `.` and `..`,
and anything with a `/` in it, which is the one byte a name cannot hold. Return
on one of those leaves the caret where it is and says nothing, because what is
wrong with the name is on the row in the user's own letters. A name something
else in the folder already has is a panel and the end of the rename: `rename(2)`
would replace that file without a word, and unlike a copy there is no "keep
both" to offer — the user asked for *this* name.

Afterwards the row goes and comes back rather than being edited in place, for
the reason a deleted file's row goes at once: what the bar holds is what was on
the disk when it was read. A folder is read again on the spot, one `readdir`,
and the cursor is put back on the row wherever the new name has moved it to —
the column is alphabetical, and a name is exactly what that order is on. A shelf
is half a million files on a worker, so the worker is told the one thing it
needs — this path is gone, that one has arrived — and the rows arrive on the
frame it has them.

A move is a rename where a rename will do, whatever the file is the size of.
The fallback is the one case it cannot be — the two paths on different
filesystems, which on any machine with a stick plugged into it is most of what
a move is *for* — and there it is a copy followed by taking the original away,
in that order, so a failure leaves the user with two copies rather than none.
Symbolic links are carried as links rather than as what they point at: a folder
of shortcuts copied the other way could be a hundred times the size of what
somebody thought they were carrying.

#### Getting what is inside an archive

Pressing a `.zip`, a `.tar.gz`, a `.rar` or a `.7z` does not start a program.
Nobody wants to *look* at an archive — they want what is inside it — and the
only part of that the shell cannot work out for itself is where the contents
should go. So the press is answered by a panel with the archive's name on it and
two ways out: **Extract here**, which means the folder the archive is sitting
in, and **Choose a folder**, which is the same mirrored bar Copy and Move are
carried on, with **Extract** at the head of every column where Paste would
otherwise be.

Those rows are recognisable before they are pressed. An archive wears a carton
with its lid on where an ordinary file wears a page — the fifth object in the
explorer's set, and there on the same rule the other four are: a mark is for a
kind the shell can do something with, and what wears it is exactly what Extract
opens, read off that one list so a row can never promise a box the press cannot
open. It is the carton the panel's own mark is emptying, shut.

It is drawn seen a little from above, which is how the drum a drive is drawn one
row up is drawn and for the same reason: the lid of a box is a face rather than
a line, and a box drawn flat-on with a lid seam across it is a mouse — two
buttons and a wheel — before it is anything else. That was the first drawing and
it had to go.

Nothing is written over, ever, and that is why the panel asks "where" rather
than "are you sure". The archive is emptied into a hidden staging directory
inside the chosen folder, and only what comes out of it is moved into place: one
thing in the box takes a free name beside its neighbours, so a `holiday.tar.gz`
holding a single `holiday/` arrives as `holiday/` and not as `holiday/holiday/`,
and a `report.pdf.gz` arrives as a PDF rather than as a folder with a PDF in it;
anything else keeps the box, under the archive's own name, so a zip of two
hundred loose files cannot empty itself over somebody's Downloads. A name
already taken takes the next free one — `holiday (2)` — exactly as a copy does.
The archive itself is left exactly where it was.

When it is done the bar is standing inside what came out. A copy or a move ends
with the column read again, because what changed is a row in a column the cursor
was already in; an unpacking makes a folder, and the folder was the point of the
press — somebody who pressed `holiday.tar.gz` wanted the photographs, not a row
saying `holiday` to step into by hand. So the shell steps into it for them, by
the walk *Show in folder* arrives by: the trail eases one column further in with
the contents under the highlight, and from **Choose a folder** it is carried
across to wherever that folder was. An archive that was one file in a coat has no
folder to stand in, so the cursor is left standing on the file it became.

The shell does not know how to unpack anything. It knows how to ask the thing
that does, which is the same arrangement removing an application uses: one
family of archive becomes one argv, run on a thread, reporting one outcome.
`bsdtar` — libarchive with a command line on it — is asked first wherever it
will do, because one program that is already under every package manager reads
tar, zip, 7z, rar, iso and cab alike; where it is missing the ladder falls to
`tar`, `unzip`, `7z`, `unar` or `unrar`, and a bare `.gz`, `.xz` or `.zst` goes
to the program that made it. Where the machine has *nothing* that can open the
format, the press is answered by a panel naming the package that would fix it
rather than by silence.

**Extract is an application as far as the rest of the machine is concerned.**
It is what the Open with list offers first for an archive, wearing a tick like
any other answer in force, and somebody who would rather press a `.zip` and get
Ark can say so there and have it stick — the choice is written into their own
`mimeapps.list` like any other, and from then on Open starts Ark and no panel
appears. Extract stays on that list underneath, so the choice can be made and
unmade; this desktop ships an entry named `linexinbar-extract.desktop` for no
other reason than that a `mimeapps.list` names desktop entries and an answer
with no name could be displaced and never chosen back. That entry is also the
road in from outside the shell: `xdg-open` on a `.zip` in a LineXinBar session
runs `lxb-desktop --extract`, which unpacks it where it stands and asks nothing,
because there is no screen there to ask on.

### Steam

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

### Covers, and the picture behind them

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

### The client, kept out of sight

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

### How it is signed in

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

### Whose client it is

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
method that has gone is answered the way a game with an agreement to accept is:
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

### With no connection

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

### Pressing a game

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

Not every question, and the ones left out are left out on purpose. An agreement
has to be read and this shell will not put an OK on one; a product key has to be
copied down; a conflict between two saves is a choice Valve shows with the date
of each, and asking somebody to pick one blind is worse than asking them to
reach for a mouse once. Those still give the client sight, the older answer,
where somebody with a pointer can deal with them.

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

Where the game goes is the client's default and deliberately not overridden: it
is the folder the user chose in Steam, and a shell that put games somewhere
else would be putting them somewhere nobody asked for. Which build comes down
is the client's decision too — this system, this account's licences, the depots
the game is actually made of. A shell that passed its own opinion in would be a
second implementation of that decision, able only to be wrong in ways Steam's
is not.

**Two games in five stop the flow to ask something.** An agreement to accept,
most often; a product key or a password otherwise. Of fifteen titles taken off
one real account, six had one — Black Mesa among them, which used to sit in
`ShowEULAs` with no window, no download and no error. Those are reported rather
than answered: the panel says what the game is waiting for and offers **Install
with Steam**, which hands the whole install to Steam's own window — the only
place the question can be put, and not one this shell will click through on
somebody's behalf. The other three in five never see Steam at all.

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

### Who is on Steam, and talking to them

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
list, back to whatever raised it. The button that raised the panel still puts
the whole thing away from anywhere in it.

A message that arrives for a conversation nobody has open raises the same
notification any program's does — the sender's face, their name, and what they
said, over two lines — and puts a count on their row. **The words are withheld
while every screen is resting**: a display the compositor has taken to black
under the OLED rule is a display nobody at this machine is watching, and one
that arrives then says only that it did. Opening the conversation marks it
read.

Four things the panel says out loud rather than by drawing nothing, because
three of them look alike and only one of them is finished:

| | |
|---|---|
| the history is coming | *Reading the conversation…* |
| it came, and there was nothing in it | *Nothing has been said yet. Say something.* |
| it did not come | the reason, and **Try again** |
| a message did not go | the message stays, in the warning colour, with the reason under it — **A** sends it again, **Options** gives up on it |

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

### Settings > Games > Steam

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

The three are written to the settings file as `steam-integration`,
`steam-at-startup` and `steam-after-a-game`, on every machine including one
with no Steam installed: what they answer is what *this shell* does. A file
that says nothing about them — every file written before this page existed —
gets the integration on, the client started for a game, and left running after
one. `--no-steam` outranks all three and does not rewrite them: a machine
booted once with the flag comes back the next morning set as it was, and the
page says so instead of offering a switch that would change nothing.

### RetroArch, and your own console games

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
of it in the pad's place ([the guide button](#the-guide-button-is-the-shells-alone)), and Steam
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
[drives it itself](#the-pad-with-no-driver-at-all), so RetroArch is given the
real thing.

None of it is kept. RetroArch writes its settings back over its own
configuration when it closes and cannot tell a setting somebody chose from a
line appended on the way in, so the file switches that off for launches the
shell makes — otherwise one evening's pad order would stand as settings for
every launch afterwards, including the ones this shell knows nothing about. Save
files, save states, playlists and each core's own options are written elsewhere
and are untouched. Deleting the file loses nothing; the next game writes it
again.

### Appearance

#### Accent color

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

#### Theme

`Settings > Appearance > Theme` is what the shell is *made of*, and it is a page
rather than a list: **Wallpaper** and **Icons**, each offering **Default** and
**Simple** — and the wallpaper one thing more, [a picture or a film of your
own](#custom-wallpaper).

Under `Default` the wallpaper's current is a band of water three sheets thick,
lit as bodies, and every one of the shell's own marks is a bead of water shaded
out of its own distance field. `Simple` stands that down — the current becomes
the three fine glass-silk ribbons the shell drew before the band, and a mark
becomes the flat shape of itself. The *drawings* never change, only what they
are made of, which is what keeps the shell recognisable rather than reduced.

The two are separate settings because they are separate expenses and separate
tastes. The wallpaper is one evaluation of a long function for every pixel of
every screen on every frame; a mark is a few dozen pixels of a row. On an
RX 9060 XT one full-screen evaluation is 0.38 ms at 1080p under `Default` and
0.20 ms under `Simple`, and a mark goes from six reads of its distance field to
one — so a machine that cannot pay for the water behind everything can very well
keep the beads in front of it, and somebody who simply prefers flat marks can
have those over the moving water.

Both rows preview: highlighting a value draws the shell in it without choosing
it, and walking back off puts the applied one back. There is no transition,
because there is no halfway between a bead of water and the flat shape of one —
and previewing is essential here, since neither value's *name* tells anybody
what they are looking at.

They are written to `~/.config/lxb/shell.toml`, which you can also just edit:

```toml
theme-wallpaper = "Default"
theme-icons = "Simple"
```

The login screen reads both keys and the compositor reads the wallpaper's, so a
machine set to `Simple` is in `Simple` from the moment the greeter appears and
never changes material in front of you. A file from before the setting was split
carries one `theme` key; it is still read, and both halves take it.

#### Custom wallpaper

`Settings > Appearance > Theme > Wallpaper > Custom wallpaper` is the third
answer under Wallpaper, and it is not a material: it is a picture or a film of
your own, standing where the shell's scene would be. Choosing it stops the
background shader drawing a scene at all — no gradient, no lights, no aurora, no
band of water — and puts your file there instead.

The row opens the shell's own file browser, on the same three places Files opens
on: your home directory, the machine from `/`, and every drive that is mounted.
It lists **only what could stand behind a screen** — the folders to keep walking
through, pictures, and films — and pictures show themselves on their rows, as
they do everywhere else in the shell. There is no context menu in there: the file
is the answer to a question, not something to be copied, renamed or thrown away
from a column you opened to choose a wallpaper. Press one and the bar comes back
out to the Wallpaper column with Custom wallpaper ticked and the file's name
under it.

**It is the wallpaper, not a layer over it.** Glass panes refract it, the guide
blurs it behind the overlay, the overview draws it inside the start screen's
card, and a game's key art still lies over it exactly as it lies over the scene.
The picture is cropped to fill the display rather than stretched into its shape.

**A film plays silently, and cannot do otherwise.** Its audio stream is never
opened, no audio decoder is ever made, and the only thing in this shell that
makes a noise is its own embedded clips — see [Libraries](#libraries). It loops,
it is decoded at its own frame rate and no faster, and it stops dead when nothing
is drawing it: an application filling the screen, or a display that has gone to
rest, costs one sleeping thread.

The file you choose is **copied into `~/.local/share/lxb/wallpaper/`**, under
its own name, and it is that copy the shell reads from then on. Tidying your
Downloads folder, renaming the picture or unplugging the stick it came off does
not take your wallpaper with it. The copy happens on a thread — a film can be several gigabytes — while the
picture you chose is already on screen.

```toml
theme-wallpaper = "Custom wallpaper"
wallpaper-file = "/home/you/.local/share/lxb/wallpaper/sunset.jpg"
```

If that file is not there when the session starts — a drive not plugged in this
morning — the shell draws its own wallpaper for the session and **leaves the
setting alone**, so plugging the drive back in brings it back. A file that
nothing here can decode, pressed just now, puts the setting back to `Default` in
front of you.

The login screen and the compositor both draw the shell's own scene for this
setting rather than your picture, and neither is being lazy: the file is under
your home directory, and both of them run before your session does — the greeter
as its own user, in front of every account on the machine. So the frame that
bridges the start of the session is the water, in your accent, and your own
wallpaper appears with the shell.

#### Battery percentage

`Settings > Appearance > Battery percentage` decides whether the start screen's
corner writes the charge out in figures over the mark that draws it. **Off**,
until somebody asks for it.

**The row is there only on a machine with a battery**, and so is the mark. What
counts as one is the kernel's own answer, read from `/sys/class/power_supply`:
a supply of type `Battery` whose `scope` is not `Device` and which is actually
in its bay. That last pair of conditions is doing real work — a desktop with a
wireless mouse on it lists a battery, and it is the mouse's; a laptop with the
battery taken out still lists the bay it came from. Neither is a machine this
corner has anything to say about, and on both it says nothing rather than
drawing an outline or greying a row out. No daemon is involved: UPower is what
a desktop environment would ask, it may not be installed, and what it reads is
this same directory.

The mark itself is not the setting. It is one of six drawings of the same
shell — empty, low, half, high, full, and a bolt for a battery that is filling
— and it is drawn whenever there is a battery, in the same water as the clock
beside it. It appears twice: in the start screen's corner, and in the
[guide's header](#the-guide-overlay) under the day. One size in both, because what
decides that size is the material rather than either layout — the shell's wall
is 2.4 units of the drawing, and what matters is how many pixels that lands on
at 1280x800. Being on the mains outranks the level: while it is filling, the bolt
is what is shown, and the level it is filling from is what the figures are for.
Sitting plugged in at full is not filling, and shows a full battery.

The figures are off by default because the mark already answers the question a
glance at a corner asks, which is *how much is left*. A number is for somebody
who wants to know whether it is 61 or 68, and a console that put one on the
wallpaper for everybody would be asking everybody to read it.

```toml
battery-percent = true
```

The key is written on every machine, a desktop included, so that a file carried
between one and a laptop does not lose the setting on the way.

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
six pages: **Resolution**, **Refresh rate**, **Orientation**, **Night light**,
**HDR**, and **OLED protection**.

Every one of them is *per screen*, and every one of them names the screen
before it offers anything — see below, where the rule is written out once for
HDR and holds for all five.

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

#### Night light

```
Settings > Display > Night light  >  DP-1  >  Schedule  >  Sunset to sunrise
```

The blue light filter: everything on a screen tinted towards a warmer colour
temperature, so a display looked at in the evening is not a daylight-white one.
It is the CRTC's gamma ramp — green and blue scaled down against red — which is
why it is the compositor's to carry out, and why it composes with HDR instead of
fighting it: both are encoded into the same ramp, in one commit, so turning one
on cannot undo the other.

Three rows on every screen that has a ramp, and five where the schedule is one
with hours in it:

| | |
| --- | --- |
| **Night light** | Off, or on. Off is the whole of off, whatever the schedule says. |
| **Color temperature** | A bar, 2000 K to 6500 K. Lower is warmer. |
| **Schedule** | **All day**, **Sunset to sunrise**, or **Custom hours**. |
| **From** | The hour of local time it comes on at. *Custom hours only.* |
| **Until** | The hour it goes off at. *Custom hours only.* |

Turned on for the first time it is an **evening**: 22:00 to 06:00, at 4000 K.
That is the one default here that can look like nothing happening — switched on
in daylight it warms nothing until ten — which is why the row's own comment
reads *On at 22:00* rather than *On*. The alternative is worse: a filter whose
whole purpose is the evening, coming on the moment it is asked for at eleven in
the morning, is a filter most people would turn straight back off.

##### The temperature is a bar

`Color temperature` is the one setting in the tree that opens onto a **bar**
rather than a list, because it is the one whose answers are a *scale*. Every
hundred kelvin between candlelight and daylight is a sensible thing to want; as
rows that is forty-five of them, which is a column nobody can scan standing for
a quantity that has no steps in it to begin with. The short list it replaces was
eight arbitrary points, and somebody who wanted the one between two of them
could not have it.

Up and Down move the value, which is what those two mean in every other column —
there is simply nowhere for a cursor to go, because the bar is the whole of its
column. **Left still leaves**, exactly as it leaves any other column, so nothing
new has to be learnt either to use it or to get back out of it. The bar stops at
both ends rather than wrapping, and a held direction crosses the whole range in
a moment.

The filled part is drawn **in the colour of the light it stands for**, which is
the one thing neither the number nor the words can be: a picture of what the
screen is about to look like. Higher up the track is more kelvin — cooler, less
filter — so a full white bar reads as what it is, no warming at all, and a short
orange one as candlelight.

The groove, the light lying in it and the handle are each a slab of the **same
glass every other control in the shell is cut from** — bevelled rim, the sheen
down that bevel and the colour split at its edge all worked out from the one
lamp the shell is lit by, not painted on. The groove *is* the row's glass: it
stands where the disc under a chosen icon would have stood, and there is no disc
under it, because a round pane behind a tall track is a button that has been
pressed with a bar lying across it.

The number is still said in kelvin rather than as a percentage of some
undeclared maximum: 2700 K is the bulb in the lamp beside the screen, and
somebody who knows that knows what the bar will do before they move it. The line
under it carries the strength in words for everybody else — *A filament bulb*,
*Distinctly warm, like a lamp*. The warm end stops at 2000 K because below
roughly 1900 K a black body has no blue in it at all, and a screen with its blue
channel taken to zero does not show blue-on-white text as warm, it shows it as
blank. 6500 K is daylight and is exactly the picture with the filter off — the
ramp is normalised so that it is the identity to the last code, rather than
nearly one.

##### The schedule

**It is the shell's, not the compositor's.** A schedule is a clock and a time
zone, and a compositor has no business owning either; so the shell works out
whether the light should be burning at this moment and sends only the answer.
Nine in the evening arriving is then an ordinary change to a value the shell was
already comparing every pass of its loop, carried out by exactly the path a
button press goes through.

**Sunset to sunrise** needs no hours at all: both ends move every day, and the
row says which they are today and where they were worked out for — *20:10 to
05:13 today, at Europe/Warsaw*. The location comes from the **time zone's own
coordinates**, out of the zone table the system's time zone data already ships.
That is deliberately the cheapest of the honest answers: a geolocation daemon
may not be installed and asks a permission question a colour setting has no
business raising, an address looked up over the network is the user's location
leaving the machine, and asking them to type a latitude is asking them to go and
find one. This is already on the disk, already theirs, and costs one file read
for the whole session.

What it gives is the zone's representative city rather than a position, which
inside a large zone can be a few hundred kilometres out — some tens of minutes
of sunset, and far inside what a night light cares about. Somebody a long way
from that city can write `night-light-latitude` and `night-light-longitude` into
`~/.config/lxb/shell.toml`; there is no page for them, because a page asking for
a latitude would be asking the user to look one up. Where the machine names no
place at all the row is replaced by the reason there is none, rather than
offering a schedule that could never come on. Above the arctic circle the two
honest readings are kept: on a day the sun does not rise the light burns through
it, and on one it does not set it does not come on at all.

**Custom hours** is the two rows below. They wrap past midnight, which is the
ordinary case rather than the exception — *21:00 to 07:00* is an evening, and
each ending hour says how long the window it makes lasts. The end is exclusive:
a light that goes off at 07:00 is off at seven. The two ends may never be the
same hour, so the **Until** page lists twenty-three of them and leaves out the
one the window starts on: a window that ends where it begins is neither a whole
day nor none of one, and there would be no way to look at that row and tell
which it meant.

Changing the schedule puts the hours aside without forgetting them — asking for
Custom hours again gives back the evening that was there, not one the shell made
up — and while they are not being kept, **the two rows are not on the page at
all**. Hidden rather than explained, because they are not the schedule's detail
so much as *one* schedule's: a page that is following the sun and still shows
*From 22:00* is making a claim about tonight that is not true, and no wording
inside the row undoes two hours sitting there in plain sight.

Which screens are listed is the compositor's answer again, and it is a much
longer list than HDR's: warming a picture needs a gamma ramp and nothing else —
no EDID claim, nothing of the link — so an ordinary SDR laptop panel that will
never do HDR is on this page. What drops off is a nested session, which owns no
CRTC and therefore no ramp; where nothing can be warmed, the row says so rather
than opening onto a column of controls that would all be inert.

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

#### OLED protection

```
Settings > Display > OLED protection  >  DP-1  >  On
```

One switch, and what it does is rest this screen behind black while another one
is being used. An OLED panel keeps what it is shown, and the start screen is
the worst thing there is to keep: the bar sits in the same row of pixels every
second it is up, the clock in the same corner, and a second display left on it
through an evening's play is a display with a bar burnt into it.

The black is the compositor's own sheet over the whole screen — the cursor and
anything running on it included — because the shell owns one surface per
display and nothing else on it. It takes nothing away: the session goes on
taking input the whole time it is down, which is what makes moving the pointer
onto the screen the way to get it back. A second going down, a quarter of a
second coming back, and it reverses from wherever it has got to.

Three things stop a screen being rested, and none of them is a setting:

| | |
| --- | --- |
| **Nothing is open at all** | Something has to be in front of one of the displays — any application, whatever started it: a game, a film, a browser, an emulator, Valve's own storefront. The compositor answers it from the window in front rather than from what that window calls itself, because an application reaches the screen as `steam_app_…`, as its own name, or as nothing at all. When the last one closes, every screen comes back. |
| **This screen is being driven** | Control is on it, so the user is on it. |
| **Something on it is still painting** | A film on the second screen is exactly what a second screen is for. A film somebody *paused* is deliberately not spared — a paused film is a still picture, which is the thing this exists for. |

What is deliberately *not* on that list is what happens to be open on the screen
being rested. A paused game, a window nobody has touched, the start screen: all
of them are still pictures, and the two clauses above already spare every screen
somebody is actually at. A session with something open on each display would
otherwise be one where no screen could ever rest.

Past those, a screen rests five seconds after the user last did anything on it:
moved the pointer over it, took it over, or pressed something on it. Going back
to what was open starts that five seconds again, so switching between screens
never blacks one out in the middle of it.

It is written down per connector, beside the mode and the night light:

```toml
[display.DP-1]
oled-protection = true
```

Nothing of it reaches the compositor's own `config.toml`. Where the mode and
the night light are remembered there — a session with no shell still has to
come up in the right mode — this is a rule about what the *shell* is drawing
and who is looking at it, and a compositor with no shell has neither.

### Sounds

`Settings > Sounds` holds two kinds of thing: where the *machine's* sound goes
and comes from, and what the shell itself plays. The devices come first, because
a session is set up before it is decorated and because nothing else on the page
can be heard until the sound is coming out of the right place.

```
Settings > Sounds > Output device  >  the cards this machine can play through
                    Input device   >  the ones it can record from
                    Start music    >  Off
```

**Output device** and **Input device** set what the whole machine plays through
and records from — every application on it, not just this shell, and whether or
not LineXinBar is running when they start. Each row lists what the sound server
offers, with the card on the line the eye lands on and how it is being driven
under it, and marks the one in use; the row above the list names that one too, so
the usual question is answered without stepping in at all. Monitors are left out
of the inputs: every output has one — its own sound, offered back for recording —
and none of them is a microphone.

This is the one setting in the column the shell does **not** write down. The
sound server is what remembers a default device, exactly as it remembers the
volume, and a shell that kept its own copy would be a second opinion about it at
every login: a device chosen in any other mixer would be quietly undone the next
time this one started. So the choice is handed to the server — `pactl
set-default-sink` and `set-default-source`, which is what PipeWire and PulseAudio
both answer — and the mark on the row is read back from it.

Choosing does not preview. Moving every sound on the machine to a device as the
cursor passes over it would put somebody's film in the wrong room four times on
the way down a list of four; the names are what the list is for, and the sound
follows when a row is chosen.

A machine with no sound server has nothing to choose here, and the page says so
rather than opening onto nothing: without PipeWire or PulseAudio nothing decides
this for the machine, and each program opens the sound card itself. A server that
answers and lists no output is a different fact — a machine with no sound card —
and reads differently.

**Start music** is the start screen's [background
music](#shell-audio) — on, which is what the shell comes up doing, or off. It is
the one recording the shell can be told not to play, because it is the one it
plays at somebody who has pressed nothing: every other sound is an answer to a
control, and a shell with a button that answered silently would be a shell with a
dead button on it.

Turning it off is heard at once rather than faded out. A fade is what an
application taking the display gets, because that is a handover; this is somebody
saying *stop*, and most of a second of music going anyway is not what they asked
for. Turning it back on starts the track from its beginning, exactly as
returning to the start screen from an application does.

It says nothing about how loud the rest of the shell is. Every click, the
keyboard and the shutter stay exactly where the mixer's `System` row left them —
that row is how loud the shell is, and this is whether one of the things it plays
exists at all. Silencing `System` still silences the music too, and turning the
music back on does not unmute anything.

Nothing about it previews as the cursor passes over it, for a reason the Display
pages do not have. Turning music off is instant and costs nothing; turning it
back *on* rebuilds the track from sample zero, so a cursor walked between the two
rows would answer with the same four hundred milliseconds of fade-in over and
over, which is not what the setting sounds like. It is written to
`~/.config/lxb/shell.toml` beside the accent:

```toml
start-music = false
```

### Network

`Settings > Network` is the third of the three pages about what a session does
with the world outside itself — the picture goes out, the sound goes out, and
this goes both ways. It is after the other two because it is the one of the
three a console can be used without.

```
Settings > Network > Wired  >  Connection             >  On
                               IP address             >  Automatic / Manual
                               DNS                    >  Automatic / Manual
                               Connection information >  IP address, router, …
                     Wi-Fi  >  Wi-Fi                  >  On
                               Networks               >  the air, strongest first
                                   Upstairs        ✓  >  IP address, DNS,
                                                         Disconnect, Forget
                                   The Cafe           >  Connect, Forget
                                   Next Door             (joins on the press)
                               Connection information >  IP address, router, …
```

The Networks column is the air and nothing else. It used to open with a **Not
connected** row, which was the answer to *which network is this radio on* for a
machine on none — and for a while the only way off a network short of turning
the whole radio off. That reason is gone: the way off a network is inside the
network now, where somebody looking for it looks. What was left was a row above
the list doing the same thing at a distance, one row of ceremony on every visit
to the page for a press most people make once. On a machine that is on none of
them nothing here is marked at all, which is the honest picture — the question
has no answer yet rather than an answer called none — and the column opens on
the strongest network in the air, which is what somebody who came here to get
connected was reaching for. A radio that hears nothing gets a line saying so
rather than a row that opens onto nothing.

**What is on the page depends on what is in the machine.** A desktop with no
wireless card has no Wi-Fi page at all; a machine with no socket has no Wired
page; one with neither has a single row saying so. That is the same rule the HDR
page is built by, and it matters more here: a Wi-Fi page on a machine with no
radio in it is not merely useless, it is a page that has somebody turning a
switch on and off looking for networks that were never going to appear.

The row at the head of it says what the machine is on — `Connected by cable —
1000 Mb/s`, `Connected to Upstairs`, `Not connected` — so the question people
actually come to this page with is answered without stepping into it. The cable
wins where both are up, because that is the one the machine is really sending
over.

**Wired** is a switch and the facts behind it. A socket with nothing plugged into
it is not offered the switch: there is nothing for On to do, and a row that took
the press and left the mark on Off would be the page arguing with something the
user can see from where they are sitting. Bringing a socket up with no profile
saved for it asks NetworkManager for the obvious thing — a wired connection that
takes its address from the network — rather than putting a form in front of
somebody who plugged a cable in.

**Wi-Fi** is the radio switch, then the air. The switch is one switch for the
whole machine however many cards are in it, because that is what NetworkManager
has; a radio switched off by a key on the machine reports as much, and the page
says so instead of offering an On that cannot happen. **Networks** lists what is
in range, the one in force first and the rest strongest first — by the five bars
a signal is drawn with rather than by the per cent, so a list somebody is
standing in reorders only when something really moved and not every time the
radio breathes — one row per name: a house with a repeater in it publishes the
same network from two radios, and they are one thing to join. Each row says what joining it takes and how well it
is heard: `Saved — 62% at 5 GHz` for one there is already a profile for, `WPA2 —
55% at 5 GHz` for one that will ask for a password, `Open — 91% at 2.4 GHz` for
one that will not.

**The network the radio is on is stepped into rather than pressed.** It is the
one row in that list whose press has nothing to do — joining a network the radio
is already on is a no-op — and it is the only row that has anything more to say,
because a wireless profile belongs to a *network*: a laptop keeps a fixed address
at the office and takes whatever it is given at home, and those are two profiles.
So that row carries the tick every value in force carries, and behind it are its
own **IP address** and **DNS** pages, then **Disconnect** and **Forget**.

That is also why the addressing is not on the Wi-Fi page itself. A card that has
been on four networks this week has four answers to *what address does this
take*, and a row up there would silently be about one of them.

**A network the machine remembers is stepped into too.** It has two things to do
rather than one — join it again, or forget it — and forgetting has nowhere else
in the shell it could live: the list is otherwise a list of what is in the *air*,
where what the machine remembers shows only as the word `Saved` in a comment. So
a remembered network opens on **Connect**, with **Forget** under it. That costs a
press, and it is the one thing on this page that takes something away; what it
buys is that somebody who typed the wrong password once has a way to make the
shell ask again. A network the machine has *never* been on keeps its single
press: there is nothing saved to remove and nothing to configure until it has
been joined once.

**Forget removes the network from the machine, not from the shell.** What goes is
NetworkManager's profile — the key that got the machine on, and any address
pinned on that network — so it goes for every program on the machine, which is
the only version of forgetting that is not a second opinion. There is no panel
between the press and the act, so the row itself carries what it costs: *Remove
this network: the password will be asked for again*. It wears the same waste bin
the context menu's Uninstall row wears, because it is the same act on a different
object.

Two things about where those rows stand. **Disconnect and Forget come after the
addressing**, because a column opens on its first row and the first row has to be
one that only opens another column — somebody stepping into their own network and
pressing Accept twice out of habit lands on `IP address`. And **a cursor the
shell moves never comes to rest on a row that acts**: pressing Disconnect turns a
four-row page into the two-row one a network the radio is off gets, and a cursor
that simply clamped to the end of what was left would put a thumb on Forget with
no press of its own in between.

**Disconnect is built for every joined network**, whether or not a profile can
yet be read for the device. It is the only way off a network in the shell, and it
must not depend on NetworkManager having got round to publishing an active
connection for a radio that is already associated.

**A password is asked for by the worker, not by the press.** Pressing a network —
its own row where nothing is saved for it, **Connect** where something is — only
ever means *join this*. Whether that needs anything typed depends on
whether NetworkManager already has a profile and whether that profile still
works, which is something it knows and the shell does not — so the press goes
down, and the panel comes back up if one is wanted, with the on-screen keyboard
under it. The same panel answers the case people actually hit: a network whose
password was changed on the router is one NetworkManager has a profile for and
cannot use, and a second or two after the press it comes back asking, with *That
password was not accepted*. Typing a new one corrects the saved profile in place
rather than replacing it, so a fixed address or a name server set on that network
somewhere else survives.

An **enterprise** network — 802.1X, the kind a university or an office runs — is
listed and cannot be pressed. It needs a user name, a certificate and a server's
agreement rather than a password, and this shell will not collect the first thing
and pretend. It is listed at all because leaving it out answers *my network is
not here* with silence.

Nothing about the page previews. Highlighting a network would take the machine
off what it is on and put it on whatever the cursor was passing — mid-download,
mid-call — and on a secured one it would raise a password panel for a row nobody
chose.

#### A static address, and where the name servers come from

Both halves get the same two pages, because a fixed address is not a thing that
is true of a cable and false of a radio. They hang in different places, and that
is not an inconsistency: a socket has one profile, so its addressing is a
property of the socket as far as anybody using it is concerned, while a radio has
one profile per network.

```
Settings > Network > Wired            > IP address  >  Automatic
                                                       Manual         ✓
                                                       Address           192.168.1.50/24
                                                       Router            192.168.1.1
                                        DNS         >  Automatic
                                                       Manual         ✓
                                                       DNS servers       9.9.9.9, 1.1.1.1

                     Wi-Fi > Networks > Upstairs ✓  >  IP address
                                                       DNS
                                                       Disconnect
                                                       Forget
```

It is called **DNS** on screen because that is what every router's own page calls
it and what anybody looking for the setting will look for. The prose here goes on
saying *name servers*, which is what the three letters stand for.

The values Manual needs are *inside* its own column rather than beside it. They
could have been rows of the page above — the night light's hours are — but that
page would then carry five rows about addressing where it now carries two, and
two of the five would be a row called `Address` standing under one called `IP
address`. Inside, each column reads as what it is: here are the two answers, and
here is what the second one is set to.

**Manual pins what the machine already has.** NetworkManager refuses a manual
profile with no address in it, so switching to Manual has to supply one, and the
only address that is not an invention is the one the interface was given — which
is also what somebody means by the press: *keep this*. A socket that has never
been up has nothing to pin, and the row says so instead of being a press that is
accepted and silently fails. Going back to Automatic changes nothing but the
method: the pinned address stays in the profile, so turning DHCP on to see
whether it works does not throw away the static settings.

**Every field says whose value it is.** The panel a typed row opens covers the
trail that would otherwise have said — so the heading names the connection as
well as the value: `Address — Upstairs`, `DNS servers — Wired connection 1`.
`Address` on its own is the same panel whether somebody walked in through the
socket on the back of the machine or through the network the radio is on, and
those are two different profiles with two different addresses.

**Name servers are only offered a choice where there is one.** Under automatic
addressing, `Automatic` takes what DHCP hands over and `Manual` uses only the
ones named below it. Under a pinned address there is no DHCP running, so there
is nothing for them to be automatic *from* — the page says that and offers the
list, rather than an `Automatic` that would quietly mean *none at all*.

**The three values are typed, and that is a third kind of row.** A colour
temperature is a scale, which is what a bar is for; an IP address is neither a
scale nor a set — it is four numbers and a prefix, of which every one is as
likely as any other, and no list anybody could write would have the user's on
it. So pressing one opens a panel with a field and the on-screen keyboard under
it, holding what the value already is: changing the last number of an address
should not mean typing the other three again.

What is typed is **checked before it is handed over**, while the panel is still
on screen and can still be corrected. `192.168.1.50` with no `/24` after it is
the thing everybody types and is not an address a profile can use — nothing in
it says how much of the network is local — so it is answered with *add the size
of the network*, in the same line, in the same place, with what was typed still
in the field. Clearing a field is always allowed and always means something:
clearing the address hands the connection back to DHCP, and clearing the name
servers hands the question back to the network.

Setting one **does not take the link down**. The profile is written and the
interface is brought into line with it through NetworkManager's own `Reapply`,
which is the call that exists for exactly this. Where that is refused the
profile has still been written and comes into force the next time the interface
comes up.

**IPv4 only**, and that is a stated limit rather than an oversight: what people
mean by "give this machine a static address" is an IPv4 address, and IPv6
addressing is a form with a great deal more in it. IPv6 is left exactly as
NetworkManager had it, so a machine given a static IPv4 address still gets
whatever the network offers it over IPv6.

**None of it is written down by the shell.** This is the second setting in the
column that is not, for the same reason the sound device is not: NetworkManager
is what remembers a network once it has been joined, every other program on the
machine reads that, and a shell with its own copy would be a second opinion about
it at every login.

**It is also the one page in the column that needs a daemon.** Everything else
the shell reaches for it reaches for directly — the kernel's backlight, whatever
sound server the session has, the connector's own colour pipeline. Wireless
cannot be: joining a network means speaking the four-way handshake against an
access point, which is a supplicant, and nothing in the kernel does it. A shell
that wanted Wi-Fi without depending on anything would have to become one, and
would then be fighting the one the machine already has running. So this page
talks to NetworkManager, and a session without it gets one row saying so —
exactly as a session with no sound server does on the page above.

Proxies, VPNs, hotspots and enterprise credentials are deliberately absent.
Those are not one press and a value, they are forms, and a console shell
offering half a form would be worse than one that says plainly that the network
it cannot join has to be set up elsewhere.

### System

`Settings > System` is the page about neither the picture nor the sound. It holds
four rows: **Application scaling**, which is the reason it exists,
**Picture-in-Picture**, which is what happens to a browser's floating video
window, **Button hints**, which is whether the start screen writes
[what its buttons do](#what-the-buttons-do) in its corner, and **System
information**, which is the page a console needs to be able to say what it is.

Button hints is under System rather than under Appearance, which is the one thing
about its place worth arguing over. What it changes is not how the shell *looks*
but how much it says about itself — the same kind of answer as how large an
application is drawn, which is the row above it, and not the same kind as an
accent colour.

```
Settings > System > Application scaling  >  150%
```

It is a bar, not a list, and the same object the night light's colour temperature
is set on: every five per cent between 100% and 300% is a sensible answer, and as
rows that would be forty-one of them standing for a quantity that has no steps in
it. Up and Down move the value, Left leaves, and a click along the groove goes
straight to the size it landed on. The row above it reads `150% — half again as
large`, so the usual question is answered without stepping in.

**100% is the floor.** The bar starts there — one to one, every application at
the size it chose — and there is no step below it: an application asked to draw
its interface *smaller* than it chose is a thing to want at a desk two feet from
a 4K panel, and this shell is driven from an armchair. A number below 100 in the
file, or from an older shell, is read as 100.

It is not a magnification. What the compositor does with it is give each
application a logical window that much smaller than the display and tell it —
over `wp_fractional_scale_v1` — that its scale is that much higher, so the client
renders a buffer with exactly as many pixels as the screen has and those pixels
are put on it one for one. At 200% on a 1280×800 display a window is configured
at 640×400, hands over a 1280×800 buffer, and its text comes out twice the size
and just as sharp — the same thing a high-density laptop panel does to every
toolkit on it. A client that ignores the scale is drawn at the size it chose and
enlarged, which is soft, and is the answer such a client gets everywhere.

**The shell is not affected.** It draws itself in layer surfaces sized against
the display it was given, so the bar, the guide and this very page stay exactly
where they are at any setting — which is the whole reason this is done per window
instead of by moving the output's own scale.

**Neither is anything under Xwayland.** X11 has no per-surface scale to tell a
client about, so the only thing that could be done to those windows is to
magnify pixels they have already drawn, and a blurred window is not what
somebody asking for a larger one asked for.

One number for the session rather than one per display, unlike everything under
Display: the two screens on a desk are looked at by the same pair of eyes from
the same chair, and a window that changed size on being moved between them would
be answering a question nobody asked. It is written to
`~/.config/lxb/shell.toml`:

```toml
application-scale = 150
```

The compositor remembers nothing about it, which is the one place this differs
from a mode or a night light. Those are written down by the compositor because it
lights the displays a second before the shell can speak and being corrected
afterwards costs a black screen; here there is nothing on screen to correct —
every application is started *by* the shell, always after it has said what this
is.

#### Picture-in-Picture

```
Settings > System > Picture-in-Picture   >  On, medium, top right
                                            Picture-in-Picture  >  Off / On
                                            Size                >  Small / Medium / Large
                                            Placement           >  the four corners
```

A browser asked to put a video into picture-in-picture opens a small window for
it, and that window is titled `Picture-in-Picture` — the one string every
browser that has the feature agrees on. It cannot be recognised any other way:
the window belongs to the browser and calls itself by the browser's name, which
is also what the window the video came out of calls itself.

A window that answers to that title is taken out of the layout every other
window here is under. It is **not maximized**, it is **not given the keyboard**,
it is **not listed in the guide** as something to switch to, and it is drawn
**in front of everything the session has** — over a fullscreen game, over the
start screen, and over the guide, which is the one surface nothing else in this
compositor is allowed in front of. That is the whole feature: a window that is
still there while the user does something else.

It is still clicked on, exactly where it is drawn, which is how its own play
button is pressed — and it is looked for *in front of* everything else for the
same reason it is drawn in front of everything else: what is nearest the hand
and what is nearest the eye cannot be two different windows. A press on it never
takes the keyboard, though. Whatever the user was working in goes on hearing
every key, which is the whole point of a video parked out of the way. And the
application it belongs to is never put to sleep while it is on screen, however
completely the rest of that application is covered — the sleeper asks whether
anything of an application can be seen, and this can.

**Size** is three shares of the display's width — a sixth, a quarter or a third
— rather than a number of pixels: the same choice has to mean the same thing on
a laptop panel and on a television across the room. How *tall* the window is at
that width is the window's own business. The compositor asks it what shape it
wants to be, by sending it a configure carrying no size at all — which is
xdg-shell for *choose one* — and follows the answer, so a four-to-three video is
drawn four to three and a phone's video stood on its end is drawn standing on
its end. Until it answers, and for a client that never does, sixteen to nine
stands in.

The answer is the first size the client draws that it was not *told* to draw,
and it is listened for as long as the window floats. Both halves of that were
paid for. A client's opening move is very often not a window at all — Firefox
commits a single pixel before it has laid anything out — and a placeholder read
as a shape says *square*, which is a widescreen video in a square frame. And a
browser will put a video of another shape into the same window, which is a
second answer to a question that a single reading would have closed.

**Placement** is the four corners and only the four corners: that is what a page
driven by a controller can honestly offer, and every corner holds the window off
both edges by the same distance — the shell's menu radius, which is what every
shape in this session is spaced by. A second picture-in-picture opened while the
first is still up stands **below it in a column** from that corner, in the order
they started floating — two windows in one corner is one window with something
wrong with it, and the one already there does not move aside for the newcomer.

**A mouse moves it.** Eight logical pixels in from each edge of
the surround is a band that resizes the window, and where two of those bands meet
is a corner that resizes it both ways; everything inside them is the video, and
dragging there carries the window. The pointer says which is which — it takes a
resize shape over the edges and leaves the client's own cursor alone everywhere
else. Neither is a client's drag: nothing is asked of the browser and nothing is
told to it.

**A press inside the frame is that window's, and nothing else's.** It is drawn in
front of everything the shell owns, so it is pressed in front of everything the
shell owns — ahead of the start screen and the guide, which are on the layer a
press would otherwise be answered by first. Without that rule a click on a video
was a click on whatever the shell had underneath it, and the user got a row
pressed they never aimed at. It stops there whether or not the client wants it,
too: a point inside the frame that no surface answers is a point *nobody* hears
about rather than one the application behind hears about, and the frame is asked
rather than the surface, since the surround is this compositor's own paint and
lies in no client at all.

The one exception is a **menu of the shell's on screen**, and not only under the
panel: a press past a menu is how a menu is dismissed, and that press has to
reach the shell to do it. A panel that could not be got rid of by clicking beside
it would be worse than a video that ignores one click — and a press outside a
menu presses nothing and only closes it, so nothing is done that the user did not
ask for.

The middle of the window has two jobs at once, so it does both. The press reaches
the client the instant it happens, so a play button answers at once, and the drag
only starts if the hand then travels four pixels — at which point the client is
told the pointer *left*, which is what cancels the click it was in the middle of.
It is deliberately not sent a release: a release is a *completed* click, and on a
video's own play button a completed click is the video stopping because somebody
moved it out of the way.

A drag never restretches the video. The opening keeps the shape its client asked
for, so all eight handles scale the window and pulling one edge moves the other
dimension with it; the edges nobody is holding stay exactly where they were. It
cannot be pulled smaller than the smallest the layout draws, or larger than the
screen, or off it — and it stays on the screen it was opened on, as every window
here does.

**A controller moves it too, out of the guide.** A console has no pointer, so
there is nothing to put on the window and nothing to press it with — but there
is a moment when the user is plainly not using the application underneath, and
that is the moment the overlay is up. **With the guide open, the right stick
pressed hands the guide's own directions to the videos floating over it**, and
presses again to hand them back; Back does the same. Nothing is offered when
there is nothing floating on that screen, and the press then does nothing at all.

The selected window is marked by the compositor rather than by the shell, which
is forced and is the right way round: such a window is drawn in front of every
surface the shell owns, so a mark drawn by the shell would be behind the thing
it marks. Its hairline surround turns the session's accent and an accent glow
breathes out into the shadow around it, on the same one-and-four-fifths seconds
everything else the user is choosing between breathes at. The guide's own
selection stops breathing while the directions are elsewhere: it stays where the
user left it and stays lit, because they are coming back to it, but two things
pulsing side by side is two controls claiming one thumb.

**And the guide steps back while they are gone.** Three things at once, because
they are one thing said three ways — *the thumb is somewhere else*. The
selection stops breathing, as above. The **frame around the selected window card
goes**: that ring is the whole of what says *this card answers the next press*,
and while the directions are on a video it does not, so a lit ring around a card
the D-pad no longer reaches is the shell lying about where the user is standing.
And the menu itself dims, a little under two fifths of the way down, over about
a fifth of a second — far enough that which of the two halves is live is
answered from the corner of an eye that is on the video, and not so far that it
stops being readable, because it is still what the user is coming back to.

The windows in the deck are *covered* for that rather than faded. Everything
else in the menu is the shell's own drawing and a fade reaches it; the windows
are the compositor's, and the shell only frames them. Fading the frame alone
would leave the brightest thing in the menu — a live window, very often a moving
picture — at full strength while everything around it went quiet, which reads as
a fault rather than as a step back. So the same share of the same dark glass is
laid over each card instead, and the whole menu arrives at one brightness.

It is a position rather than a start time, in both directions, so a guide that
gets its directions back before it has finished stepping away comes forward from
where it is instead of snapping the rest of the way out first.

**The right stick then carries the window**, exactly as a mouse button held on it
does — the same arithmetic, the same limits, the same leaving of the column. It
takes hold by itself, without a button to hold down, and lets go when the stick
comes back to rest. The D-pad and the left stick walk between the videos on that
screen, by **where they are** rather than by what order they were listed in: they
stand in a column until somebody moves one, and after that they are wherever they
were put. A direction with nothing that way moves nothing, which is what pushing
into the end of any other list here does. Only that screen's, because the guide
is only ever on the screen being driven and a window belongs to the screen it
opened on; the videos on the other screen are reached by taking the guide there
with the shoulder buttons.

**The top face button raises the window's own menu**, which is the menu the right
button raises, about the same window, with the same rows. Everything else on the
pad goes on meaning what it means everywhere — the guide button is still the way
out of all of it, the shoulder buttons still move between displays, the volume is
still about the machine and the camera still photographs whatever is in front of
the user.

**One thing is allowed over such a window, and it is a menu about it.** A window
made large enough covers the very menu that offers to make it small again —
awkward with a mouse, and a dead end on a pad, where there is no pointer to find
an unseen row with. So a context menu is drawn in front of the floating windows
rather than behind them, and the pointer follows the picture: a press on the
panel is the shell's, and one beside it is still the window's.

**The menu gets a surface of its own** for this, a child of the display's, and
nothing else is drawn on it. That is the part that had to be learned. A panel is
rounded and a rectangle is not, so lifting the panel's bounding box out of the
shell's main surface laid four square corners of the start screen over the video.
Given a surface of its own the panel is exactly its own shape, everything around
it is transparent, and the video shows through to its edge. The compositor is
told which surface it is and nothing more: it never learns where the panel is or
what shape it has, and the surface's own input region is what decides whose a
press is, so what the eye finds and what the hand finds cannot drift apart.

The surface is a *subsurface*, so it commits with the display's own and no frame
can show the panel without the wash behind it or the wash without the panel. It
is made the first time a menu is opened on that display, and its buffer comes
back off when the last one goes — a session where nobody opens a menu pays
nothing for any of this.

**The right button raises a menu** — the same menu the rest of this session
raises, drawn by the shell, because a compositor cannot draw the shell's glass.
The compositor asks for it and is told what was chosen. Eight rows: *Move*,
*Resize*, *Realign*, *Full screen*, *Move to next display*, *Move to previous
display*, *Close*, and *Cancel* in a band of its own.

*Move* hands the window to the pointer: the pointer is put in the middle of it,
the window follows until the next click, and a click of any other button puts it
back where it was. *Resize* does the same from a corner — the one with the most
screen behind it, which is where the room to grow is — with the pointer put on
that corner so it follows the hand rather than the window jumping to it. Nothing
is held down through either, because the button that chose the row was let go of
before the window ever moved.

**On a controller those two rows are the same two acts with the stick instead.**
Nothing is warped, because there is nothing to warp: the window follows the right
stick from the moment the row is chosen, Accept leaves it where it ended up and
Back puts it back where it started. Resize is the only way to resize a floating
window with a pad at all, since the stick alone moves it.

**The directions move the window inside that mode**, rather than walking the
selection off it — walking it off is what they must not do, since it would leave
a window being carried by nobody, but doing nothing at all was the answer to
that only for as long as the one control here was a stick. A keyboard has no
stick. So a direction is a step: eight logical pixels for the first press, and a
key held down accelerates over about fourteen repeats to sixty-four, which
crosses a 1080p display in about three seconds from a standing start. A step
small enough to place a window to the pixel takes half a minute to cross a
screen and one large enough to cross a screen cannot place anything; the ramp is
what has both. It eases rather than climbing at a constant rate, and a different
direction or a gap longer than a held key's starts the run again at walking
pace.

A resize pulls **the corner the compositor chose** — the one with the most room
behind it — so the direction that makes a window in the bottom-right larger is
up and left. That is the stick's behaviour exactly, and deliberately: which
corner is held is a fact about where the window is standing on a display only
the compositor has laid out, and a shell that mapped the keys some other way
would be guessing at it.

*Realign* undoes both of them: the window goes back to the size the Settings
page asks for, in the corner it asks for, wherever it had been dragged or pulled
to — and it springs there rather than snapping, the same spring the column moves
on when a video arrives in it or leaves it. A window already standing in the
column is already all of that, so the row simply confirms it.

*Full screen* takes the window out of the corner altogether and gives it the
display, as an ordinary application window: **maximized, listed in the guide,
holding the keyboard, closed and switched to like every other one**. It grows out
of its corner to get there, over the same three-tenths of a second and along the
same flight a window makes when the guide brings it back from a tile, because
what the row asks for is this window opened full screen and that is what opening
one full screen looks like here. The flight waits for the client: what it grows
*from* is the corner, and what it grows *to* is the window at its new size, so it
begins on the first frame the client has actually drawn that size rather than on
the frame it was told to.

**What sends it back is on the menu of the deck it has just joined.** The
guide's own window menu — the one raised on a card, in the table of
[context menus](#the-context-menu) — carries *Open as Picture-in-Picture*, which
is this row read the other way: the window leaves the layout and goes to sit in
the corner, with the surround, the size and the column a browser's video gets.
The two are one request in two directions, and what it carries is nothing more
than *which windows float*.

So it works on any window at all, which is the honest consequence of that rather
than a second feature: the user is a better judge of what belongs in a corner
than a title is. And what they say outranks the title from then on — a browser
goes on calling that window `Picture-in-Picture` after they have said they want
to watch it properly, and a session that read the title again would put the video
straight back in the corner it had just been taken out of.

The row is drawn only while this feature is switched on, and absent rather than
greyed when it is not: a window given a corner on a session with no floating
windows would have no menu to be got out of the corner with. That is the one
thing a switched-off Settings page has to keep true.

The two display rows send the same request the guide's own window menu sends,
and follow the same rule: neither wraps, and the row that cannot be taken is
disabled and still drawn, so the menu keeps its shape on every screen. A move
between screens changes the screen and nothing else — a window still standing in
the corner joins the new screen's column, and one that had been dragged keeps
the place it was put in. *Close* asks the window to close, which is what puts
the video back in the page it came from; it is deliberately not the Close the
guide offers for an application, because that one ends the application and the
application here is a browser with the rest of somebody's session in it.

**A window that has been moved leaves the column.** It keeps where it was put, it
is no longer counted in the corner it came from, and the place it used to stand
in is free — the next video that starts floating takes it, and a window below it
moves up into it exactly as it would have if the window had closed. Two things
put a moved window back: the *Realign* row above, which is one window saying so,
and the Settings page, which is all of them — choosing a size or a corner there
is the user saying where they want their videos, and every floating window
returns to the column when they do.

The window is drawn with a hairline surround — **three pixels, fixed** — and a
shadow, its corners rounded at eight. The surround is not decoration. A client's
buffer is a rectangle with four square corners and nothing in a renderer can cut
a curve out of one, so the corners are *covered* rather than cut: the window is
configured to the opening in the surround, and the surround is painted over its
edges — rounded on the outside at the full radius, rounded on the inside at what
is left of it.

**That is why the two numbers are one decision.** The client's square corner sits
√2·(r − b) from the centre of the outer arc, so it is hidden only where
√2·(r − b) ≤ r: a surround as thin as three pixels can only round a corner about
eight, and rounding it further at that thickness brings the four corners of the
client's buffer out through the curve. It is the shell's menu radius that this
window is held *off the edges of the screen* by, not what its own corners are
rounded at — those were one number while the surround was a share of the radius,
and a hairline frame would have parked the window three pixels off the edge of
the display. The arithmetic is in `crates/lxb-protocol/src/pip.rs`, shared with
the compositor that paints it so the two cannot disagree about the shape.

**Nothing is drawn outside that shape.** Two different clients make that worth
saying. One draws its own decorations — Firefox's picture-in-picture window is a
GTK window with its own rounded corners and its own drop shadow, spilling tens
of pixels outside the window proper, which without this hangs out past the
surround and is exactly the leak the surround exists to prevent. The other
simply does not take the size it is given, or has not taken it yet. So the
window is scaled down to fit its opening if it is drawing larger than one —
shrink only, the way the overview scales a window into a card — and then clipped
to it.

**And nothing shows through it.** The surround is painted a shade *over* the
picture, the way a mat lies on the edge of a photograph rather than beside it,
because nothing at that edge lines up: the opening is a fractional rectangle —
a quarter of the display's width inset by a third of a radius that is a share of
its height — while the client is configured at a whole size, its buffer lands on
whole pixels, and the surround's own edge softens itself over the one pixel it
falls in. Butt those together and half a pixel belongs to nobody, which on screen
is a one-pixel line of whatever the user is really doing, down one side of their
video. Half a pixel is enough, now that both halves take the opening from the
same arithmetic: it is what a fractional display scale leaves in the resampling,
and it antialiases the surround's inner edge the way its outer edge always was.

Behind the window, the opening is filled with the surround's own colour, for the
other way a hole opens: a client that does not cover it at all — one still
starting up, one that will not take its size, one with transparent corners of its
own — is centred and letterboxed on more surround. What is behind a floating
window is the application the user is actually using, and any of it seen *inside*
the frame reads as the window being broken rather than as something showing
through.

**It arrives and it leaves.** A window that simply appeared in a corner at full
size would read as a glitch rather than as something the user asked for, so it
fades up out of nothing over 240 ms while it grows the last tenth of the way to
its own size — eased out, so it is already travelling when the eye finds it and
slows into place. Going takes 180 ms and is the same two numbers read the other
way: it fades out and falls back a tenth, smoothstepped, so the last frame drawn
of it is worth nothing at all rather than a fifth of a window blinking off.

The frame, the picture and the colour behind the picture are one shape and go
through **one** transform about one origin — the middle of the window — rather
than three that agree by arithmetic; a surround three pixels thick has nothing
to spare for two halves rounding a corner to different pixels. What it costs is
a pixel: while the window is being scaled, the client's own drawing is cut one
physical pixel inside its opening, so that a pixel lost to rounding is a pixel
of *surround* over the video rather than a square corner of video outside the
curve. What shows in its place is the surround's own colour, which is what is
behind the video anyway.

**And the column springs.** Two things move a floating window that nobody is
dragging: another window arriving under it or leaving above it — which is what
happens the moment somebody pulls one out of the column, and the column closes
up behind it — and a press on the Settings page changing how large these
windows are or which corner they sit in. Both take 480 ms and both are a
*spring*: a decaying cosine that goes about a tenth of the way past its
destination, comes back about a hundredth short of it, and lands. One good
bounce and the ghost of a second, which reads as something soft rather than as
something sprung.

It lands *exactly*. The wobble is three and a half half-turns because a cosine
is exactly zero there, so the curve arrives on its destination at the end of the
480 ms rather than a fraction of a percent short — and a fraction of a percent
of a five-hundred-pixel window is two pixels appearing from nowhere on the last
frame of the animation, which is the jump the spring exists to remove.

The window's own picture is scaled per axis while it springs, so a window
springing into a shape of another proportion squashes on the way. That is the
same one transform everything else goes through, so the surround, the video and
the colour behind the video cannot come apart.

Three windows never spring, and each for its own reason. One with a **hand on
it** — dragging or resizing — is where the hand is, and only while the hand is
still on it: a drag cancelled puts the window back the way everything else
moves. One still **arriving** is already animating, and a browser answering what
shape it wants to be a few frames in must not turn that into two animations at
once. And one being laid out for the **first** time has nowhere to spring from.

Leaving is the harder half, and it is worth saying why. **A browser does not
stop calling its window picture-in-picture when the video goes back into the
page — it destroys the window.** A destroyed surface has no buffer, no texture
and no state, so a fade drawn from the window itself would have nothing to draw:
the video would vanish on the first frame and the surround would fade out around
a hole. So the compositor keeps a note of what each floating window last looked
like — the texture handles the renderer already made, which outlive the client
that gave them, and the numbers that say where on the screen they go — refreshed
every frame at the cost of a reference count per surface, and nothing at all on
a session with no video parked in a corner. When the client goes, that note is
what is faded out. A renderer that did not take the note draws the surround
fading with nothing inside it, which is the case on a second GPU and nowhere
else.

**Off** is the honest answer for somebody who does not want a video following
them around: such a window becomes the application window it otherwise is —
maximized, listed, focusable — which is what this session did before it could be
asked. It is written to `~/.config/lxb/shell.toml`:

```toml
picture-in-picture = true
picture-in-picture-size = "medium"
picture-in-picture-place = "top-right"
```

Carried out by the compositor, which is what places windows, and remembered
nowhere else: the shell says what this is as soon as it connects, which is long
before any browser exists to put a video in — the same bargain the application
scale above it is under.

#### System information

The last row of the page opens a panel rather than a column: the shell's own
fennec mark over nine named facts about the machine, read at the moment it is
pressed and dismissed with `Close`.

```
Settings > System > System information

    System name        Some System
    System version     9
    System software    Version 0.1.0
    IP address         192.0.2.17
    Kernel             Linux 6.6.0-generic
    Processor          Some Core X9-9000 6-Core
    Graphics           Some Radeon X9000
    Memory             20 GiB free of 30 GiB
    Disk space         120 GiB free of 233 GiB
```

**System name** and **System version** are `NAME` and `VERSION_ID` out of
`/etc/os-release`; a rolling release publishes no version, and that row is left
off rather than shown empty. **System software** is this shell's own version —
the `VERSION` file at the root of the checkout, which both halves are built
against. **IP address** is the first ordinary address on an interface that is up
and is not the loopback, asked of the kernel rather than of a network manager,
since a session with no desktop in it has nobody to ask; a machine on no network
reads `Not connected`. **Kernel**, **Processor** and **Memory** come from
`/proc`, and **Disk space** from the filesystem the system is installed on.
**Graphics** is the adapter the shell itself is drawing through, which is the one
thing here no file on the disk knows.

Nothing on the page can be changed, and nothing is invented: a value the machine
will not give reads `Unknown` rather than something derived from the value beside
it. The processor and the adapter are named as their vendors name them, less the
`(R)` and `(TM)` marks. Two more words come off, both of them a label repeating
itself at the cost of the end of the row: a trailing `Processor` on the CPU, and
the trailing bracket in which Mesa writes the driver and chip codename rather
than the card.

### Several displays

Each display gets its own bar, not a copy of one: they browse independently and
remember where they were. Only one takes input at a time — the others are
dimmed and drop the footer, so it is obvious which one the controller is
driving. `L1` / `R1`, or `Tab` / `Shift+Tab`, hand control to the next display,
and a click on a display takes it there; the compositor's first output has it at
startup, since Wayland has no notion of a primary display. Nothing else moves
it — a pointer merely crossing onto the other screen leaves control where it
is, because a mouse on its way somewhere else is not a decision about which
screen the user is using.

An application launched from a display opens on that display, because the shell
names that display before starting anything. It does not leave the compositor to
work it out from keyboard focus: focus has usually moved on to something else by
the time the new window maps.

It names it twice, and the two answers are different questions. `place_launch`
files that one press — the display it was made on, the process the shell forked,
and the class the window will carry — so the window it turns into opens where it
was pressed whatever else has been started since. Two screens can be loading two
things at once, and a single answer for the session named one of them: the two
windows arriving in either order both went wherever that one answer happened to
point, which put a game on the screen nobody had pressed it on and left the other
screen waiting out its patience for a window standing next door. `set_launch_output`
is still sent, and is now the fallback for a window that answers to no record.

`set_driven_output` is the other one, and it is about the person rather than
about a window: which display hands the keyboard back when something closes,
which display's game is the X screen's active window, and which display a
screenshot binding photographs. It was the same field until the two came apart,
and read as "where the user is" the launch answer walked all three off to
whichever screen was loading — for as long as it took to load. A game being
played on the other screen was then told by the root window that it was in the
background, which every Proton title reads.

And it stays there. The display a window is placed on is recorded on the window
itself, so every later re-tile — a sibling window closing, the client asking to
be maximized or fullscreened, an X11 client moving itself — puts it back on the
same screen. Nothing moves a window between displays but a user asking for it:
`Super+Shift+→`, or the two move rows in [the menu](#the-context-menu) over its
card in the guide. Without that record the placement would be re-derived from
the window's bounding box, which a window that has mapped but not yet drawn does
not have: an application whose
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
| `Alt+Tab` / `Alt+Shift+Tab`                 | Walk [the guide's cards](#the-cards) while Alt is held; letting it go switches to the one the walk landed on |
| `Esc`, `Backspace` or controller `B`        | Step out, then open the guide overlay |
| `Home`, `Super`, mouse side button, controller Guide/STEAM button | Open the guide overlay |
| `Y`, `F10`, right mouse button, controller `Y`/`Triangle` | Open [the context menu](#the-context-menu) on what is selected |
| `P`, controller right stick pressed, with the guide open | Hand the guide's directions to the [videos floating over it](#picture-in-picture), and hand them back |
| `Print` (with anything held), `Ctrl+Shift+3`, `Alt+Shift+3`, controller Guide/STEAM + `R1` | [Photograph](#screenshots) the display being driven |
| Controller Guide/STEAM + Select/View | Ask [Valve's own overlay](#the-guide-button-is-the-shells-alone) to come up over the Steam game in front |
| The volume keys, with anything held | Turn [the session](#quick-settings) up or down a step, or silence it |

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

One pad escapes the gamepad API entirely. The second-generation Steam
Controller has no kernel driver — `hid-steam` claims the original, its receiver
and the Deck, and this one falls through to `hid-generic` — so it has no
joystick node at all and the gamepad API enumerates nothing. It is read from
its hidraw report instead, which is the only source that survives both of the
states it has: on its own it is in the firmware's lizard mode, pretending to be
a keyboard and a mouse, and the moment Steam is launched Steam claims it and
writes lizard mode off. Reading hidraw is not exclusive, so it works alongside
Steam's own reads, and it is opened read-only — leaving lizard mode means
*writing* feature reports, which is Steam's business. The compositor drops the
pad's lizard keyboard so the same button cannot arrive twice.

Every button press is logged at debug level with its mapped name, its raw code
and which of the two namings it came under, which is the fastest way to work
out what an unusual pad is actually sending:

```sh
RUST_LOG=lxb_desktop::controller=debug lxb-desktop
```

Controller initialisation failure is non-fatal and leaves the keyboard usable.
Pass `--no-gamepad` to skip controller discovery entirely — which also turns
off [the stick pointer](#the-stick-pointer), since there is no stick to read,
and leaves every pad's guide button alone along with the rest of it.

### The guide button is the shell's alone

The compositor holds every other spelling of the guide back from the focused
application outright — the `Home` key, `Super`, the mouse's side button, both
edges of each. It is the way *out* of an application that is holding everything
else, and an application that could take it over would be an application there
is no way out of.

A controller cannot be held back that way, because a controller never passes
through the compositor at all. There is no gamepad protocol in Wayland, so a
game opens `/dev/input` itself and sees exactly what the shell sees. So the pad
is taken away and given back with one button missing: LineXinBar grabs each
pad's device node — `EVIOCGRAB`, which makes every other reader of it deaf —
and puts a `uinput` device in its place that says it is the same pad and
repeats everything it does except the guide button. Applications find the
stand-in where they would have found the pad, and the only reader of the real
one is this shell.

The stand-in is the same pad in every respect an application can ask about:
name, bus, vendor, product, version, every button, every axis with its range
and resolution, and force feedback, which is passed back the other way so a
game can still shake the real pad. The guide button is *declared* there and
never sent — SDL builds its controller GUID from the identity and numbers
buttons by walking the capability bitmap, so a stand-in that differed in any of
that would be a pad the mapping database has never heard of, with every button
in the wrong place.

Three rules keep this from costing more than it is worth:

- **A pad is never taken without being given back.** The grab and the stand-in
  are made together, and if any part of it fails — no `/dev/uinput`, no
  permission, a node that never appears — the grab is dropped and the pad is
  left exactly as it was found. Losing the rule is a leaked button; losing the
  pad is a console nobody can play.
- **A pad another program has already grabbed is left alone**, for the same
  reason.
- **Nothing that can type is ever taken**, however many gamepad codes it also
  declares, and nothing `uinput` made — which is this shell's own stand-in,
  Steam Input's pad for a game, or another session's.

The one route this cannot cover is `hidraw`. A pad that speaks HID has a raw
report node too, reads of it are not exclusive, and there is no kernel
interface for making them so — SDL will read a pad that way in preference to
`/dev/input` when its HIDAPI drivers recognise it. So every application the
shell starts is handed `SDL_HIDAPI_IGNORE_DEVICES` naming *the pads this shell
is standing in front of*, and only those: a listed pad still arrives complete
through `/dev/input`, and a pad with nothing standing in for it is never
listed, so nothing is ever asked to ignore the only route a controller has.
Anything already in the environment is added to rather than replaced.

**Valve's client is handed a shorter list than everything else**, and the
difference is one pad. A controller the guard is holding is on both lists: the
client reads `hidraw` around a grab like any other program, and the stand-in is
waiting for it on `/dev/input`. A controller with *no kernel driver* — the
second-generation Steam Controller, below — is on neither the client's list nor
anybody's road to it but this shell's, because there is no `/dev/input` node to
fall back to. Naming it to the client does not move the client onto the
stand-in; nothing moves the client onto anything. It takes the pad away from
the client altogether, and from every game the client launches. Measured on the
pad this was written for: `SDL_hid_enumerate` returns five interfaces of
`28de:1304`, none at all with the id on the list, and the client's own log goes
from five `Local Device Found` lines to none.

#### The pad with no driver at all

The second-generation Steam Controller reaches the same place by the opposite
road. `hid-steam` does not claim it, so the kernel makes it no gamepad node —
there is nothing to grab, and nothing for a game to find either. LineXinBar
drives that pad itself: it reads the pad's report off `hidraw` and **builds the
gamepad the kernel did not**, a `uinput` device of an Xbox controller's exact
shape, so that SDL, GilRs, RetroArch and any game find a complete controller
where they look for one.

The Steam button never reaches that device. It is declared on it, for the same
button-numbering reason as above, and it is the one control the driver keeps —
which needs no grab to enforce, because a stand-in this shell builds is only
ever told what this shell chooses to tell it. The pad's own raw node joins the
ignore list handed to *applications*, now that there is somewhere else to read
it — and stays off the one handed to Valve's client, which is the other half of
this pad's road to a Steam game.

**Valve's client is the one reader that cannot be shut out**, and it is worth
being plain about why. Steam reads this pad's raw HID reports with its own code
rather than through SDL, so the ignore list above does not reach it, and a raw
node cannot be held exclusively by anybody — there is no `EVIOCGRAB` for
`hidraw`. Every other controller escapes this by having a device node to take
away; this one has none, which is the whole reason the shell had to build it
one. So the button reaches Steam whatever the shell does.

What is left is to ask Steam not to act on it, and Steam has a setting for
exactly that — *Guide button focuses Steam*, on its Controller page. The shell
turns it off through the same interface it signs the client in through, every
time it wakes the client, because the client keeps that setting in memory
rather than on disk. Without it, every press of the guide button opens Big
Picture behind the shell. Measured three presses either way: with the setting
on, Steam goes to Big Picture on the first press; with it off, its window list
does not move.

**And what the shell takes, it gives back as a chord.** The guide button is the
button Steam's overlay comes up on everywhere else a machine is shaped like a
console, so taking it and offering nothing in its place would be this shell
deciding that nobody may reach Steam's friends list, its browser or its guides
while a game is running. **Guide + Select** — `STEAM`+`View` on a Steam
Controller or a Deck, the PlayStation button and Create on a DualSense, Guide
and View on an Xbox pad — asks for the overlay over the game in front. The two
middle buttons, side by side, reachable with one thumb, which matters more here
than it does for the screenshot chord: this one is pressed while playing.

There is nothing to *ask* for it. Valve's whole client interface was read off a
live build, and the overlay is only ever something the client is told about —
`SteamClient.Overlay` registers for activation requests and reports state, and
nothing in it raises one. The overlay is not the client's to raise: it lives
inside the game, in the library Steam preloads into it, and what that library is
watching for is a keystroke. So the shell sends the keystroke, over
`lxb_shell_v1.keyboard_key`, which puts it on the seat's own keyboard exactly
where a real Shift+Tab would land.

Which keystroke was read off the same live client rather than assumed:
`overlay_key` is `Shift+Tab` — keysym 65289 is `XK_Tab` — and
`gamescope_guide_hotkey`, the same question asked for a machine that *is* shaped
like a console, is the same two keys again. They go out as Linux key **codes**,
which is what the protocol carries, and here the two cannot disagree: Tab and
Shift are in the same place on every layout xkb has, which is a large part of
why Valve could pick them.

Two refusals, and both are silent. Nothing but a game Steam started may be sent
it — in a browser the same keystroke walks the focus backwards, and a chord that
did something different in every application is a chord nobody can press with
confidence — and the shell must not be holding the keyboard itself, because then
the key would be delivered to the shell. Neither is worth saying on screen: the
first is the ordinary state of a machine with no game running, and a message
every time somebody brushed two buttons in a menu would be worse than nothing
happening. The third failure *is* said, because it is the one nobody could work
out alone: a client that has moved its overlay to another key, or switched it
off altogether, is read on every wake and written to the log — the shell sends
Valve's default, and a moved overlay would make the chord silently dead.

The triggers are analogue over the pad's own fifteen bits rather than the
single byte `xpad` gives one — every reader scales by the range a device
declares, so declaring the true one costs nothing and keeps the part of a
trigger that is not a button. The stand-in has no rumble: shaking the pad means
writing to it, and the driver does not.

The driver is deliberately **read-only**. Leaving lizard mode, rumbling, the
gyro and the trackpad haptics all mean *writing* to the pad, which is what
Steam does when it claims it, and two programs configuring one controller is
one controller doing neither. Reading only is what lets the shell and Steam
hold the same pad at once — so with Steam running the trackpads, the gyro and
the haptics keep working through Steam, and with Steam closed the trackpads
still move the pointer as the firmware's own mouse. The lizard-mode *keyboard*
that would otherwise deliver every button a second time is dropped in the
compositor.

The shell reads this pad from its report and not from the gamepad it made: the
report's layout was captured on the hardware, so which button is which is known
exactly, where a mapping database asked about a pad it has never heard of gets
two of the face buttons the wrong way round. Everything else on the machine
reads the gamepad.

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
| Volume mixer      | Opens [a panel](#the-volume-mixer) of everything making a noise, a row per application |
| Do not disturb    | Whether anything may interrupt. On, an announcement is filed without a bubble and without a chime |
| Notifications     | Opens the list of what has been announced to the session, newest first. Wears a mark while anything on it has not been looked at |
| Volume            | How loud the session is — a bar, moved with Left/Right; `A` mutes. The [volume keys](#quick-settings) move this same control from anywhere |
| Brightness        | How bright *this* display is, where that can be changed |
| Resume            | Dismiss the overlay |
| Close *app*       | End the application on the selected card. It is asked first and cannot refuse |
| Start screen      | Show the start screen over the running application, without closing it |
| Power             | Suspend, turn off, or end the session |

Close and Start screen are offered only while the card beside the column is a
window. The start screen is the last card in the deck, and it is neither
something to close nor something for Start screen to bring up that Resume does not
already. Nothing but the power button ends the session: `Esc` opens this menu
rather than quitting, so leaving is always a deliberate choice.

Close names the application on the *selected* card rather than the one in
front, and ends it rather than asking it to go: it is sent the polite close a
window manager sends, and `SIGTERM` then `SIGKILL` four seconds later if that
is ignored. What is ended is the application rather than the window's own
process — killing the window Steam draws leaves `steam` to put up another one —
so the compositor works out what the application is before it works out how to
end it. The grace is there so an application can finish writing, not so it can
decline.

The first four are square tiles sharing one line at the head of the column
rather than rows of their own, centred on the panel: Up and Down treat the line
as one stop on the way down the column, Left and Right walk it, and a tile that
can do nothing from where the user is standing is stepped over rather than
stopped on. Only the stick pointer is ever in that state, because it is the one
tile about the application in front — the mixer always has the session's own
output on it, the notification list answers *nothing arrived* as readily as it
lists what did, and whether the session may be interrupted is a question with an
answer on an empty machine.

The bell carries the one mark in the column: a bead in its corner while
something has been announced that nobody has looked at, the badge every phone
puts there. No number on it — what is worth knowing from across a room is
*something arrived*, and how many there are is a question the list itself
answers, one row per line, a press away. Opening the list is what counts as
looking at it; a bubble in the corner of the screen deliberately does not,
because it appears whether or not anybody is in the room. The mark is white
rather than accent-coloured so that walking the selection onto the very tile it
is pointing at does not make it disappear into the light.

Two of the four are switches, and a switch says which state it is in by being
*filled* rather than by being lit, since the selection is already lit. Do not
disturb is the one that outlives the application it was thrown over: it is
written to `shell.toml` and comes back with the next session, because a console
that had quietly turned it off overnight would deliver a night of announcements
at breakfast. What it stops is the bubble in the corner and the sound that goes
with it — one answer, given in one place, so neither can be silenced without the
other. Nothing is discarded: everything still arrives, and the tile beside it is
where it is read.

The header is two columns. On the left the wall clock, and under it whatever is
running on this display; on the right the day, and under *that* the battery — so
the left of it is what the session is doing and the right is what is true about
the machine underneath. On a machine with no battery the right-hand column is
the day and nothing else.

It is the same mark and the same reading the start screen's corner draws — see
[Battery percentage](#battery-percentage), which is the one setting behind both.
What differs is where the figures go when they are turned on: **left of the
mark** here, and above it in the corner. The corner is a cluster on a wallpaper
with a whole display beside it, where stacking the number keeps it from pushing
the clock inward; this is a narrow column with a line of its own to spend and
nothing above the mark but the date. A long application name gives up exactly
the room the mark takes and ends in an ellipsis rather than running under it.

### What is coming down

The far corner from the column, and the one thing here that is not part of the
menu: while a Steam game is downloading, a card stands in the bottom right
wearing the game's own icon, with what is arriving, a bar, and the percentage.
It is a reading and not a control — no selection stops on it and no press
reaches it — and it is drawn only while this menu is open. It is described with
the rest of the download in [Steam](#steam), where the numbers on it come from.

### The cards

Beside the entry column stands a second one: a card per window on this display,
one above another, with the start screen as the last card of all. Right leaves
the entries for them and Left comes back; Up and Down step the cards, which stop
at their ends rather than wrapping, and the selected card is always centred with
its neighbours peeking in past the top and bottom edges — the cut-off card is
what says there is more to scroll to.

`A` on a card goes back to that window, so the guide is also the window
switcher; `A` on the last card is the bar, over the running application or
plainly if there is none. Close, and [the menu](#the-context-menu) the top face
button raises, are both about the *selected* card rather than about whatever is
in front.

`Alt+Tab` is that switcher reached the way every desktop reaches it. The chord
brings the overlay up with the deck already in front and the highlight on the
card *behind* the one on screen; each further `Tab` while Alt is held steps one
card on, `Alt+Shift+Tab` steps back, and letting Alt go takes whatever the walk
landed on — that application coming to the front with the keyboard, or the start
screen where the walk ended on the last card. The walk wraps, alone among the
ways of moving in the deck: a held modifier is a question rather than a held
direction, and a fourth `Tab` on three applications is asking to come back round
to the first, not to be told there is nowhere further to go. The compositor
watches for the modifier coming up, because by then the overlay it is being let
go of over is the shell's and the application is no longer being sent the keys.

They are the live windows rather than pictures of them. The compositor animates
each window into its slot and draws it there, out of the same layout crate the
shell decorates the slots from, so the two processes paint one composition
without either sending the other any pixels; leaving flies every window back to
where it was. Which is also why opening the guide wakes everything on the
display up — see [What draws, and when](#what-draws-and-when).

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

The volume also has the keys it is printed on: `XF86AudioRaiseVolume`,
`XF86AudioLowerVolume` and `XF86AudioMute`, whatever is held down with them.
They work from wherever the user is — the compositor holds those bindings and
forwards them over `lxb_shell_v1`, because a fullscreen application owns the
keyboard and is usually the thing being turned down. Held down, the two
directions repeat at the session's own key repeat rate; mute is a switch and is
thrown once.

A press moves the same control the guide's row moves, by the same twentieth,
and raises it at the foot of the display for a second so there is something to
read while it moves. That control is the sidebar's own bar on a pane of its
own; while the guide is open it does not appear at all, because the row in the
sidebar is already showing it. Nothing else of the shell comes up with it — the
start screen stays behind the application, and the keys neither give the shell
focus nor swallow a click.

The overlay is drawn by the shell, not the compositor, so it uses the same
renderer and fonts as the bar. It moves its layer surface to the overlay layer
with `Exclusive` interactivity and draws onto a transparent surface, which is
what lets the running application stay visible through the scrim.

There is one overlay, on the display holding control — the other displays carry
on showing their own bar. `L1` / `R1` and `Tab` / `Shift+Tab` work from inside
the menu, and it follows control to the next display; with more than one
display it names the one it is on.

Everything it says is about that display: the cards are the windows *there*, it
offers to resume or close one of those, and it reads *Nothing is running* on a
display that has none, whatever is on the others.

Two input paths reach it, because neither alone is enough:

- **Controllers** are read straight from `/dev/input` rather than through
  Wayland, so the guide button arrives even while a game holds the keyboard —
  as do the three chords spelled on it and on Select: the on-screen keyboard's,
  [the screenshot's](#screenshots), which is wanted in that state more than in
  any other, and [Steam's own overlay](#the-guide-button-is-the-shells-alone),
  which means nothing in any other state at all. Every other control is ignored there, so the bar cannot react
  behind a running game. That the game cannot read the guide button *at all* is
  a separate piece of work on the pad itself — see
  [the guide button is the shell's alone](#the-guide-button-is-the-shells-alone).
- **Keyboards** go to the focused application, so the shell would never see the
  key. The compositor therefore owns the `guide` binding and forwards it over
  `lxb_shell_v1` (see below).

### The volume mixer

The tile beside the stick pointer's opens a panel of everything the machine is
playing: one row per application, each under the name and the icon the bar
already knows it by, with the shell's own audio in a band at the foot. Left
and Right slide a row, `A` silences it and brings it back, and the panel stays
up while they do — a track answers by *changing*, and a panel that folded away
on the press would take the answer with it.

A row is an application rather than a sound. A browser with three tabs playing
has three streams open in the sound server and is one thing anybody wants to
turn down, so they are grouped by the program behind them and the row moves all
of them together — which also means a tab falling silent does not renumber the
panel under the user's hand. What the row is called is what the desktop entry
that installed it is called, not what the stream calls itself: the bar has the
name the user chose it by, and the server has "Zen" or "Music Player Daemon".

It is [the same panel](#the-context-menu) every context menu is drawn as,
because it is the same kind of object — a short list about one control, grown
out of that control. What makes it a mixer is the rows.

The row at the foot, `System`, is [the shell's own audio](#shell-audio) — how
loudly the interface answers and the start screen's background music plays. It
is not the machine's output, and that is deliberate: what the whole session
comes out at
is the volume bar a few rows above it in the same sidebar, which is there
whether or not this panel is opened, and a row that turned the machine down as
well would be the same control twice while leaving the shell's own audio with
none. It is also the one row here that no sound server knows about, which is why
it is in a band of its own and why it is written to `shell.toml` rather than
left with the mixer. Sliding it is its own preview: the click a direction makes
is heard at the level that direction has just moved it to.

That row is always there, which is why the tile is never dimmed — the
applications come and go with what the machine is playing, and the shell is the
one thing on the list that is certainly there, being the thing drawing it. The
application rows come from `pactl list sink-inputs`, which PipeWire answers as
well as PulseAudio; a machine with neither — the `amixer` case in
[Quick settings](#quick-settings) — gets a mixer with the shell's own row on it
and nothing else, because per-application volume is not something the kernel
mixer has.

The shell is in that listing too, as `ALSA plug-in [lxb-desktop]`, and it is
struck out of the applications: it is already on the panel as `System`, and two
rows over one thing would be one control too many — the one a user reached for
first being the one that does not last, since the server forgets a stream the
moment it closes and the shell opens a new one for the next click. It is
recognised by the process the server names it under, and, on a server that
names no process at all, by the program in the brackets the ALSA plug-in puts
it in.

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
that cannot be thrown. [The mixer](#the-volume-mixer) beside it never is,
whatever is running, because the panel it opens always has the session's own
output on it. Both are still drawn, because a line that came and went with the
application would slide the other tile across the sidebar every time something
was closed.

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
PlayStation one — which is where a console shell of this shape has kept its
options menu since the first one. On a keyboard it is `Y` or `F10`, deliberately *not* the
`Menu` key: that one summons the guide, and with Steam running it is the only
thing that does so without Big Picture coming up alongside it.

It is a component rather than a screen. Whatever raises it hands over three
things: the rectangle of the control being acted on, a name for it, and the
rows. The panel then grows out of that rectangle, stands beside it on whichever
side of the display has room, and folds back into it when the menu is answered
or dismissed. Adding a command later is one line in a list; raising a menu
somewhere new is one function that returns those three things.

Six of them exist so far:

| Where | What it is about | Rows |
| ----- | ---------------- | ---- |
| The bar | The application on the focused tile, out of the disc it stands on | Information, Uninstall / Launch, Close |
| The bar | One of the user's own files, on a shelf | Open, Open with, Delete, [Rename](#changing-a-name) / Sort, Cancel |
| The bar | One of the user's own files, inside a folder listing | Open, Open with, Delete, [Copy, Move](#carrying-a-file-somewhere-else), [Rename](#changing-a-name) / Sort, Cancel |
| The bar | A folder inside a listing, out of the same disc | Copy, Move, Rename / Sort, Cancel |
| The guide | The window under the selected card, out of that card | Move to next display, Move to previous display, [Open as Picture-in-Picture](#picture-in-picture), Screenshot the app / Cancel |
| The guide | [Everything making a noise](#the-volume-mixer), out of the mixer tile | One row per application / the session's own output |
| Anywhere | A [floating window](#picture-in-picture), out of the window itself | Move, Resize, Realign, Full screen, Move to next display, Move to previous display, Close / Cancel |

The last is a menu in shape and material and not in kind: its rows are tracks
rather than commands, so it is slid rather than pressed and it is raised by `A`
on the tile rather than by the button below. Nothing about the panel had to
change to carry it, which is the point of it being a component.

A row can also lead to a *further* list rather than doing something — Open with
and Sort both do. The panel stays exactly where it is and swaps what is written
on it, once the row that asked has been seen going down; `B` steps back out to
the list it came from, and only closes the panel from the outermost one.

The rows themselves are ordinary furniture. A row can carry a glyph, sit in a
band of its own below a hairline, be warm for something there is no coming back
from, or be offered and out of reach — "Move to next display" is drawn as an
outline on the last display rather than being left out, so the command is still
discoverable and the menu keeps its shape on every screen. The highlight steps
straight over anything it cannot stop on, wraps at both ends, and glides between
rows rather than jumping. A list
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

**Every one of them is drawn on a surface of its own**, a child of the display's,
with nothing else on it. There is one context menu in this shell at a time and
this is where it goes, whichever of the seven raised it — which is what lets the
compositor put it in front of a [floating window](#picture-in-picture), the
one thing otherwise drawn over everything the shell owns. A window somebody has
made large enough covers the very menu that offers to make it small again, and on
a pad, with no pointer to find an unseen row with, that is a dead end. The
surface's own input region is cut to the panel, so what the eye finds and what
the hand finds are the same shape, and nothing else about the panel — where it
is, how large, what shape — ever reaches the compositor.

The panel's glass shows what is behind it wherever that is, and it is the same
answer for all seven: what the shell drew itself it reads back out of its own
frame, its wallpaper it evaluates from the function that painted it, and what
another client drew it is handed a small picture of by the compositor — see
[What a pane of glass shows](#what-a-pane-of-glass-shows).

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

## Screenshots

`Print` — the key with a picture of it on it — photographs the display the
keyboard is on, and it does so **whatever is held down with it**. Every desktop
spells a different variant of the picture with a modifier (the whole screen on
`Shift+Print` here, the clipboard on `Ctrl+Print` there, a region on
`Meta+Shift+Print`) and this shell takes one kind of picture, so a hand that
learnt any of those spellings is right. For a keyboard with no Print key at all
— a sixty-percent board — there is the chord every Mac has had for thirty
years, in both of the ways it gets transcribed onto a PC: `Ctrl+Shift+3`, with
Command read as Control, and `Alt+Shift+3`, with Command read as the key that
sits where it sits. Those two are ordinary chords and need their modifiers
exactly, because the key under them is a digit and belongs to whoever is
typing.

On a controller it is the **guide button with `R1`**, held together: the same chord a Steam Deck photographs a game with, and
the same two controls under every thumb — `STEAM`+`R1` on a Deck or a Steam
Controller, the PlayStation button and `R1` on a DualSense, Guide and `RB` on an
Xbox pad. It photographs the display that has control, which is the one the
shoulder buttons move between. The bumper on its own still moves to the next
display; a chord rather than a button of its own because there is no spare
control on a controller, every face button and both shoulders already belonging
to whatever is running.

The guide button is what pays for that chord: it is answered when it comes back
*up* rather than when it goes down, so that holding it can mean something. A tap
still opens the overlay and is still a tap; a hold spent on the chord opens
nothing at all. The alternative would be an overlay already drawn over the game
by the time the bumper arrived, and a photograph of the overlay.

It pays for two chords now — the other is [Steam's own
overlay](#the-guide-button-is-the-shells-alone), spelled with Select — and one
hold is spent by whichever of them lands. One hold and not one per pad: the
guide button is a single control however many controllers are plugged in, and a
chord is answered once rather than once for every device that could have spelled
it.

What lands in the file is what was on that screen: the wallpaper, the
application over it, and the shell's own bar or overlay over that, at as many
pixels as the display is being driven at. The pointer is not in it. The cursor
is drawn by the compositor rather than being part of anything, it is off screen
entirely while the session is driven from a controller, and an arrow burnt into
a picture cannot be taken back out of it.

The guide's menu over a window card offers the other picture: [Screenshot the
app](#the-context-menu), which is that application on its own — its own
contents at its own size, with nothing in front of it and no wallpaper behind.

Both land in the `Screenshots` folder inside the user's pictures — the folder
`xdg-user-dirs` recorded in the language the account was made in, so `Bilder`
on a German installation and `画像` on a Japanese one, never a second English
`Pictures` beside the real one. They turn up in the shell's own
[Images](#music-video-and-images) row like any other photograph on the disk.

Nothing pops up to say so. The display **flashes** instead, once, and the shell
sounds `screenshot.ogg` beside it — both after the file has actually been
written, which is why the flash is never in the picture it is answering for and
why the pair mean the screenshot happened rather than that a key was pressed.
Two senses because the chord is often spelled without looking: a person watching
the screen sees the flash, a person with their eyes on the game hears the
shutter. A panel would have to be drawn either behind the fullscreen application
it was reporting on, where nobody would see it, or in front of it with the
keyboard taken off whatever the user was doing.

Only the compositor can take either picture. A Wayland client reads its own
surfaces and nothing else, and the shell is a client: it cannot photograph even
the application it is drawn over. So both are requests over
[`lxb_shell_v1`](#lxb_shell_v1), with the compositor supplying the pixels and
the shell the folder.

## Screen sharing

OBS records this session and Discord shares it, the same way they do on any
other desktop — which is to say through three pieces that have to all be there,
because none of them can do the job alone.

**The compositor speaks `wlr-screencopy-unstable-v1`.** A client asks for one
frame of one display, or of a rectangle of one, is told what buffer to bring,
and hands it over; the copy happens the next time that display draws. That is
also what paces a recording, so nothing has to guess the refresh rate. What
lands in the buffer is the composite, out of the same element list the display
draws, and whether the pointer is in it is the client's choice —
`copy_with_damage` really does wait, so a recorder pointed at a still screen is
given one frame and then nothing until something moves. Any tool that speaks
this protocol works: `grim`, `wf-recorder`, `wl-screenrec`.

**`lxb-portal` is the session's xdg-desktop-portal backend.** No application
speaks Wayland to share a screen: it asks `xdg-desktop-portal` over D-Bus, and
that hands the question to whichever backend the desktop installed. GNOME ships
one, KDE ships one, and this is LineXinBar's — `org.freedesktop.impl.portal.ScreenCast`,
answering with the number of a PipeWire node.

**The frames go over PipeWire, and are copied once.** Every buffer in the
stream is a memfd the portal allocates, and a memfd is exactly what a
`wl_shm_pool` is made from — so the memory PipeWire hands the application is
the same memory the compositor was told to copy the display into. The picture
goes from the GPU to the application in one step; nothing in the portal ever
touches a pixel.

The three files that make a session find it are in
[`share/`](share/xdg-desktop-portal): the `.portal` file that names the D-Bus
backend, the `linexinbar-portals.conf` that says this backend answers screen
sharing and file choosing and leaves the rest to whatever else the machine has,
and a D-Bus service file so anything asking early can start it. The compositor
starts it beside the shell under `--shell`, because a portal is a client of
this compositor and has to be given the session's own display.

**Those files have to be installed, or nothing ever calls it.**
`xdg-desktop-portal` chooses its backends by reading `.portal` files out of the
data directories, and it does so once, at startup — a session whose portal was
never registered has `lxb-portal` sitting on the bus answering nothing, while
every application that asks is told there is no ScreenCast portal at all. That
is not an error anybody is shown: OBS simply offers no screen-capture source
and Discord's picker never appears. A package installs them; a session run
straight out of a checkout installs nothing, and
[`scripts/install-portal.sh`](scripts/install-portal.sh) registers that build
for the current user instead (`--uninstall` takes it back). The portal says so
in the log when it cannot find its own registration.

**And the session has to tell the bus what it is.** A D-Bus activated service
inherits nothing from whatever asked for it — it is started by the bus, from
the bus's own snapshot of the environment — so a session that never updates
that snapshot has the portal come up with no `XDG_CURRENT_DESKTOP` and no
display, which lands in exactly the same place: a front desk with no screen
sharing on it. The compositor updates it (through
`dbus-update-activation-environment --systemd`) as soon as it owns the seat, or
when `LXB_PRIVATE_DBUS=1` says it has been given a bus of its own; nested on
somebody else's bus it leaves the snapshot alone, because rewriting it would
send *their* activated services into this compositor.

The other half of that is giving it back. The portal is one service per user
rather than one per session, and nothing restarts it, so a portal left running
with LineXinBar's answers is inherited by whatever the user logs into next —
and screen sharing is broken *there* instead, which is how this was found. A
session that owns the seat therefore stops `xdg-desktop-portal` on its way out,
and `lxb-portal` holds a connection to its own compositor for no other reason
than to exit when that connection breaks.

`lxb-portal --list-outputs` names the displays, and `lxb-portal --debug-cast
[DISPLAY]` shares one without a portal or D-Bus in front of it and prints the
node — which is how the pipeline is proved by hand with `gst-launch-1.0
pipewiresrc`.

**Nothing is shared until the user says so.** The portal cannot draw — the
shell owns the renderer, the glass and the panel every other question in this
session is asked through — so the question goes over `lxb_shell_v1`: the
compositor carries it to the shell, which opens the guide and puts up a panel
naming the application and offering one row per display, and carries the answer
back. The refusal is the row the panel opens on, so a press on a question
nobody read is a no. So is a question nobody answers, a session with no shell,
and a display unplugged between the question and the answer: there is no path
through `consent.rs` that shares a screen because something was missing.

One question at a time. The panel is modal and takes every button while it is
up, so a second application asking while the first question is on screen is
refused rather than queued behind a panel the user cannot see.

## Choosing a file

A browser wanting a photograph to upload, an editor wanting somewhere to save,
a game wanting a folder for its mods: none of them opens a file dialog of its
own on a Wayland session. It asks `xdg-desktop-portal`, which asks whichever
backend the desktop installed — usually without the application knowing,
because GTK's and Qt's own file dialogs quietly become portal calls when there
is a portal to call. This is LineXinBar's:
`org.freedesktop.impl.portal.FileChooser`, answering `OpenFile`, `SaveFile` and
`SaveFiles`.

**It is a session's own chooser because every other one needs a mouse.** GNOME's
and KDE's are windows with a tree, a list and a text field, and not one of those
is reachable from a controller. This session already has a file explorer that
is — the Files column — so the question is answered in *that*, framed.

**The panel is the explorer, at eight tenths of the display.** Cut from
[the context menu's own glass](#the-context-menu) through `sidebar_surface`, so
it is the same material as every other surface this shell raises. What is inside
it is [`crate::files`](crates/lxb-desktop/src/files.rs) and nothing invented:
the same listing, the same folders-first order, the same search field at the
head of a column, the same New folder row where the folder can be written to.

**And it is drawn as the bar draws itself**, by the very same `pick_row` the
[folder picker](#carrying-a-file-somewhere-else) uses: a mark that grows under
the cursor with a bloom breathing behind it and a disc of glass beneath it, the
name beside it, and the line under the name only where the row is chosen. The
cross is the bar's cross, read against the panel instead of the display — every
column's chosen row sits on one line, so the trail reads straight across, and
the list slides under it rather than paging. It walks the same way too: Right
steps into a folder and opens a column beside the one it came from, Left steps
back out, and the trail of columns *is* the path. A panel of its own kind of
row would have been a second visual language for the one job this shell already
has a language for.

**A photograph is drawn as itself**, in the round hole its mark would have had
— the very shape a picture met in a folder wears on the bar, made by the same
worker for the rows around each column's cursor and no others. Which picture is
this is the question a chooser exists to answer, and a column of identical page
marks cannot. A row whose picture has not arrived yet, or never will, keeps the
mark for its kind; nothing about the row's size or place depends on the answer,
so the picture fades into a row that was already there.

**The foot says where you are, in full.** The whole path of the folder being
stood in — `/home/somebody/Downloads`, not `Downloads`, because a column headed
with a folder's name is one of several folders on the disk called that and what
the application is handed is a path. Under it goes what is showing, or what the
file will be called. Where a path is too long for the panel it is the one run in
the shell cut from the *front*: every path on a machine begins the same way, and
one cut at the end names the disk and never reaches the folder. Each column's
own heading sits over its **mark** rather than over its names — the mark is where
a column begins and what every row of it lines up on.

**The pane brings its own ground.** Every other pane in the shell is laid over
something the shell drew — the wallpaper it chose to be dark, or its own bar
over it. This one is laid over whatever the application that asked happens to be
showing, which on a video or a game is bright, moving and nobody's choice. So it
is cut to a panel's frost rather than the sidebar's (`FROST_PANEL` exists for
exactly this: *a panel has to carry text over anything at all*) and laid on a
sheet of near-black in its own shape. Neither is opaque: what is behind still
comes through, still moves, and is still recognisably the application waiting.
Over a test pattern filling the display, the two together took the variation
across the panel's own face from 31 luminance levels to 11.

Eight tenths rather than the whole display is the point of it: the application
that asked goes on drawing and goes on being seen round the edges, which is what
says it is still there and still waiting. The surface is lifted over that
application and holds the keyboard — every button belongs to the panel until the
question is answered, and the letters typed into its fields must not reach the
application underneath — but it is never called opaque, so the application is
not put to sleep behind it.

**Three things are the panel's own, and each because the question came from
outside.**

*A head row that answers.* For every purpose but "one file" the answer is not a
row of the listing — it is the folder being stood in, or the set that has been
ticked — so every column carries a row at the top that ends the question. The
same shape the [folder picker](#carrying-a-file-somewhere-else)'s Paste row has.
A column never *opens* on it, for that picker's reason: it is the one row that
acts, and a press of Accept out of habit would hand an application a folder
nobody chose.

Saving is the exception, and it is the only one. Walking into a folder in order
to write into it is a walk whose whole point is the folder just reached, so a
save opens the column on **Save here**, with the name already under it — and on
the name itself where there is nothing to call the file yet, because that is the
thing standing between the user and the answer. The rule holds everywhere else,
where the folder is only the way to the row being chosen and pressing it again
would answer with something nobody picked. A save cannot lose anything that way:
what it does is write a file under a name the user can read on the row above.

*The kinds.* An application may say it only accepts images; when it does, the
files that are not are left out, which kind is in force is written at the foot,
and the panel's own menu — the same button that raises one anywhere else, and
the right mouse button anywhere inside the panel — carries them under one row
called **Types**, which says what is showing and steps into the list. One row
rather than one row each: an application may offer five filters, and laid out
flat they pushed Sort and Show hidden files off the bottom of a short panel,
which is a menu that hides its own controls the more the application asks for.
**Everything** is always the last row of that list: a filter is the
application's guess at what the user wants and it is sometimes wrong, and a
chooser with no way past its own filter is one that hides the file the user is
looking at.

A kind is matched with `fnmatch`, brackets and all. That is not a nicety: a
filter is written for whichever matcher the toolkit that sent it uses, and
Firefox spells **every** filter it sends as a bracket per letter — an upload of
a photograph asks for `*.[pP][nN][gG]`, because that is how a case-insensitive
`*.png` is put to a matcher with no other way of asking. A matcher that read the
brackets as characters answered every one of those questions with an empty
column, which is what a folder full of photographs looked like here until the
filters were read off the session bus and the tests written from the capture.

*Nothing here destroys anything.* There is no Delete, no Rename, no Copy and no
Move. An application's file question is not a file manager, and a panel raised by
a web page should not be one press from emptying a folder. New folder is the one
exception, and only where the answer is somewhere to *write*: a save that cannot
make a folder is a save that can only ever go where something already is. The
trash is not offered either, for the reason no walk that is choosing something
offers it — an answer that disappears the next time the trash is emptied.

**The foot says what the buttons do, and names the ones in hand.** Four acts —
Select, Approve, Options, Cancel — each drawn as the control that performs it
rather than lettered, because the same act is South on a pad and the space bar
on a keyboard and no wording covers both: "press A" is wrong on a PlayStation
pad and meaningless to somebody typing. So the panel draws whichever the user
last reached for; see `settings::controller_in_hand`. Approve is absent where
there is nothing to approve — choosing one file is answered by pressing the file
— and a legend naming a button that does nothing would be worse than naming
none. **Options is on the legend because without it there was nothing on the
panel to say the menu existed**: an application asking for images showed a
folder of images and no sign that the sort order and the rest of the disk were
one press away. Its keyboard half is a mouse rather than a key, which is the one
place the legend leaves the keyboard — a menu is raised with the right button by
anybody holding a pointer, and no key printed on a keyboard says the same thing
to as many people.

Where a key is named it is named **as the key is labelled**: `Esc` is a word on
every keyboard ever made, so the mark is that word, while Enter and the space
bar carry the symbols printed on them. An earlier cut drew Escape as an arrow
leaving a keycap and it named nothing anybody could go and find.

**Where you are stands above the rule**, with the width of the whole panel to
itself. It shared the foot with the legend once, and a path sharing a line with
pictures of buttons is a path cut short to leave room for them.

**Accept and Approve are two different acts here**, which they are nowhere else
in the shell. Accept — South, or the space bar — presses the row under the
cursor: it steps into a folder, ticks a file, puts a name in the field. Approve
— Start, or Enter — ends the question with what has been chosen, from wherever
in the walk the cursor is standing. That is what makes a save two presses rather
than a walk back up the column to the row that answers, and it is why Enter and
the space bar part company on this one screen.

**Three ways out, and one of them is not announced.** Back cancels. A click past
the panel cancels — clicking past a thing to dismiss it is what every panel on
every desktop does, and one that ignored it read as one that had stopped
responding. And opening the guide cancels, silently: the guide outranks every
screen the shell draws and every grab an application can take, so a question
that could hold it off would be the one screen with no way out of it. Nothing is
said about that last one, because a panel explaining that the guide had
cancelled something would be the guide apologising for opening.

**Nothing is handed over until the user says so.** The road is
[`ask_to_share`](#lxb_shell_v1)'s exactly: the portal cannot draw, so the
question goes over `lxb_shell_v1`, the compositor carries it to the shell over
the top of whatever is filling the screen, and carries the answer back as paths
the portal turns into `file://` URIs. Back cancels, and so does a question
nobody answers, a session with no shell, a shell that goes away mid-question,
and a second application asking while a panel is already up. All of them reach
the application as the same thing: it was given no file.

It will not interrupt a journey the user is already in, either. A file being
carried to a folder and a panel waiting on an answer are both the shell holding
a question of its own, so an application that asks over one of those is told it
was given nothing and may ask again. The guide and a context menu are not
journeys and are simply taken away.

`lxb-portal --debug-pick [one-file|many-files|folder|new-file]` puts a question
to the shell without D-Bus or an application in front of it and prints what came
back — the counterpart of `--debug-cast`, and how this half is proved by hand.
`--kind 'Images=*.png'` offers a kind, `--at` opens it somewhere, `--called`
names a new file.

**And the registration has to name it**, which is its own trap. The `.portal`
file is installed separately from the binary and `xdg-desktop-portal` reads it
once at startup, so a backend that has grown an interface since the last install
answers a front desk that has never heard of it — and the application is quietly
handed another backend instead, which is exactly what a session with no portal
of its own looks like. That is not hypothetical: this shipped against a
registration naming ScreenCast alone, and every Save dialog in the session
opened GTK's. `lxb-portal` now compares what it answers against what its own
registration claims and says so in the log when they disagree.

## Showing a file

The other direction, and the one the desktop had backwards. A browser finishing
a download offers **Show in folder**; an archiver that has just unpacked
something offers **Open containing folder**. Neither opens a file manager. Each
calls one method on one well-known bus name:

```
org.freedesktop.FileManager1.ShowItems(["file:///home/…/thing.zip"], "")
```

**On a machine with a normal desktop installed, that name is activatable.**
Dolphin, Nautilus and Thunar each ship a D-Bus service file claiming it, so a
session that answers nothing does not get silence — it gets Dolphin, started on
demand, drawn over the shell, in a session with no window management for it and
no way back. That is what pressing Show in folder used to do here.

**So the shell holds the name.** It takes `org.freedesktop.FileManager1` at
startup, on the session bus, beside the notification daemon and the polkit
agent — and for the same reason all three are the shell rather than a process
next to it: Files is not a program to start, it is a column of the bar, four
rows into System, drawn by the shell out of the shell's own tree. Nothing else
in the session can put it on a screen or knows which display the user is
driving. A name already owned is never activated, so nothing else is started
behind it. The name is taken without replacing an existing owner, so a
LineXinBar run inside somebody else's desktop for testing leaves that desktop's
file manager alone.

**And then the shell walks there.** The cursor is carried into Files, the disk
holding the path is chosen — the deepest one, so a download opens under Home
rather than four columns down from Root — and one column is read and stepped
into per part of the path. Only when it has arrived does the bar come forward,
over whatever application was in front. That order is the point: what the user
sees arrive is the folder they asked for, rather than the bar arriving on
whatever it was last left on and then being seen to rummage through the disk.
The columns still slide open, because that is what the bar does when a column is
stepped into; nothing was invented for this.

The file itself is left under the cursor, with its size and the date it was
written on the row — which is also this desktop's answer to `ShowItemProperties`,
the one method here that does something other than what its name says. There is
no properties window in this shell, and what one would be opened to read is
already on the row. `ShowFolders` stands *in* the folder instead of pointing at
something in it.

**Three methods, one place to stand.** Several URIs in one call come down to the
first: the shell has one cursor per display and one row can be under it. A URI
that is not `file://`, a path that is not absolute and a name that is not on
this disk are each refused on the bus, on the call that made them, rather than
becoming a bar pointed at nothing. The escaping is undone as *bytes* — a file
name on Linux is bytes, and decoding into a string first would refuse every file
this shell can list whose name is not UTF-8.

It will not interrupt a journey the user is already in, on exactly the terms the
[file chooser](#choosing-a-file) will not: a file being carried, a panel waiting
on an answer, or an application's own file question each own the columns this
walk would move, and each was started by the person at the machine — where this
was started by a program behind whatever is on screen. Nor over a launch splash
or a window still flying home, both of which own the display until they hand it
over themselves.

One thing is read differently from anywhere else in the shell. **Show hidden**
is off by default, and a walk to something under `~/.local/share` with it off
reaches a column where the very folder it needed next was left out — it would
stop three levels short with nothing under the cursor, which is the shell
appearing to have lost a file it was handed the whole path of. So a path with a
dot-name anywhere in it is read with the dotfiles in, for that walk and no
other. Nothing is written down and the setting is not touched.

**The other road is `xdg-open` on a folder**, and that one is not a bus call. It
looks up whatever the machine says opens `inode/directory` and starts it, which
is also what `xdg-desktop-portal` falls back to when no file manager answers.
Two files cover it, both in [`share/applications/`](share/applications):
`linexinbar-files.desktop`, which claims the type and whose `Exec` is
`lxb-desktop --show-in-files %u` — not a shell starting, but one call to the
shell this session already has, and out — and `linexinbar-mimeapps.list`, which
names it as the default.

That list is deliberately *desktop-specific*. A file called
`<desktop>-mimeapps.list` is read only while `XDG_CURRENT_DESKTOP` lowercases to
`<desktop>`, and LineXinBar's session sets `XDG_CURRENT_DESKTOP=LineXinBar`. So
installing this package takes folders away from nothing else on the machine, a
KDE session on the same disk keeps Dolphin, and the user's own `mimeapps.list`
is never written to. A package installs both files; a session run straight out
of a checkout installs nothing, and
[`scripts/install-file-manager.sh`](scripts/install-file-manager.sh) registers
that build for the current user instead (`--uninstall` takes it back). The bus
name half needs none of that — the running shell takes it either way.

## Authorisation prompts

Mounting somebody else's disk, installing a package, restarting a service: none
of those are done by the program the user pressed. It asks `polkitd`, which
reads the action's policy, and where the policy says a human has to agree,
`polkitd` asks *the session's authentication agent* for proof. A desktop with no
agent registered is a desktop where every one of those is refused with nothing
on screen to allow it, which is what this was until the shell grew one.

**The agent is the shell itself.** It is the one part of LineXinBar with a D-Bus
connection of its own — the portal owns the rest of the session's D-Bus, and
deliberately — and the reason is what travels back: a *password*. The shell holds
one in a single allocation that never moves, never prints it, and overwrites it
when it is dropped; carrying it across a Wayland connection, through the
compositor and into a second process would undo all three. So `polkitd` is
answered on a thread of its own, and what is typed goes from the field straight
down the helper's socket without leaving the process.

**The password is checked by polkit, not by the shell.** Every agent hands it to
`polkit-agent-helper-1`, which is the only part of this that runs as root and the
only part that talks to PAM — over `/run/polkit/agent-helper.socket` where the
machine's polkit is socket-activated, or by running the setuid helper where it is
not. The helper tells `polkitd` directly whether the password was right, so
nothing the shell can get wrong turns a "no" into a "yes": the most a bug here
can do is fail to ask.

**The panel is the one a removal uses**, because it is the same question: a
padlock, what the authorisation is for in polkit's own words, whose password is
wanted, and a field drawn as one mark per character. The on-screen keyboard comes
up with it — on a console there is nothing else to type a password with — and the
guide is opened underneath, so the question is readable over a game filling the
screen. It opens on Cancel: a panel that appears without being asked for must not
have the answer that hands over authority sitting under somebody's thumb.

A password PAM refuses is offered again, exactly as a removal offers a refused
`sudo` password again, and PAM's own complaint is what the panel says when it has
one. A helper that fails *before* asking for anything is a machine that could not
be asked at all, and says so rather than looping. Cancel, Back and Escape all
decline, and a question `polkitd` withdraws — because whoever asked gave up —
takes the panel away with it. Every one of those routes ends the helper, which
leaves PAM without the answer it was waiting for.

Two things are worth knowing. **A second question waits** rather than being
refused: the shell asks one thing at a time, and the next is put up as soon as
the panel is free. And **the agent registers for the login session** where there
is one, falling back to the shell's own process — which is what a machine with no
`logind` gets, and what a LineXinBar started inside another desktop gets, since
`polkitd` allows one agent per session and that desktop's got there first.

## `lxb_shell_v1`

Layer-shell says nothing about either half of the problem above, so
[`crates/lxb-protocol`](crates/lxb-protocol) defines a small private
protocol generated from one XML file for both sides:

**It is privileged, and the compositor enforces that.** The global is offered
only to the programs the session is made of — the shell the compositor started,
and `lxb-portal`, which draws its questions through the shell — and a client
that may not have it is not shown it in the registry at all; binding it by
number anyway is a protocol error. The compositor decides from the credentials
the kernel stamps on the connection when it is accepted, never from anything a
client says about itself.

The credential is the pid, and only ever the pid of a process this compositor
started itself and has not yet reaped. That is why `lxb-portal` is kept as a
real child and started again when it stops: a portal the *bus* activated has a
pid the compositor never learns, and one it cannot recognise cannot ask the
shell anything. What the pid is decidedly not is the *name* of the executable
behind it. Every program in the session runs as the user — Valve's client and
every game it starts, whatever the software hub installed, a browser — and any
of them can copy itself to a file called `lxb-portal`. A name is not a
credential, and this protocol hides windows, closes them, types, clicks,
captures the screen and logs the user out.

**The two are not given the same interface.** The shell may use all of it. The
portal may use `offer_kind`, `ask_to_pick_files` and `ask_to_share` — the three
requests that put an outside application's question to the user — and receives
the answers to those and nothing else. Every other request on a portal's object
is a `not_permitted` protocol error, and the state the compositor broadcasts
(what each window is, its title, the process behind it) is sent to shells only.

`--insecure-trust-program NAME` puts the old rule back for one session, for
working on the shell or on `lxb-portal --debug-pick` beside a running
compositor. It is named for what it is: any process of this user can take a
name, so a session started with it has this protocol open to all of them.

This used to be on wlr-layer-shell's footing — any client of the session — on
the reasoning that the compositor only runs what the user's own session started.
That reasoning was the mistake. A LineXinBar session deliberately runs a great
deal nobody here wrote or can vet: Valve's client and every game it launches,
whatever the software hub installed, a browser. The requests below hide any
application from the screen while leaving it running, close anybody's window,
inject key presses and pointer clicks, and end the session.

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
| event `output_window` | One window on one display: a stable handle, its title and its logical size. A batch of them ends in `output_windows_done`. |
| event `output_window_app_id` | What one listed window's application calls itself, which is the only thing a running application can be recognised by. |
| event `output_window_pid` | Which process drew one listed window. The only thing about a window that is not a name somebody chose, and what lets a game be told from whatever else appeared while it was loading — see [Steam](#steam). |
| event `unseen_window` | One window the compositor is keeping off the screen on the shell's own instructions. A batch of them ends in `unseen_windows_done`. The counterpart of `output_window`, and separate from it because a hidden window is not on a display as far as anything else is concerned. |
| request `let_this_window_be_seen` | Let one window of a hidden application through without giving the application back — for a program the shell runs unseen that has stopped to ask something. See [Steam](#steam). |
| request `set_output_overview` | Enter or leave the window overview on one display, which is what draws the cards. |
| request `set_overview_selection` | Which card the shell is on, so the compositor scrolls the column the same way. |
| request `activate_window` | Raise and focus one window: how the overview doubles as a window switcher. |
| request `activate_window_from` | The same, flown in out of a rectangle — the tile an already-running application was pressed on. |
| request `kill_window` | End one window's application. Not a request it can refuse; see [the guide](#the-guide-overlay). |
| request `move_window_to_output` | Put one window on another display. |
| request `capture_window` | Photograph one window into a PNG at a path the shell chooses. |
| event `window_captured` | Where that picture went, or that it did not happen. |
| event `screenshot`  | The compositor's screenshot binding fired, and on which display. |
| event `volume`      | A volume key was pressed: one step up, one step down, or the switch that silences the session. A key held down arrives as a run of them. |
| request `capture_output` | Photograph a whole display — everything on it — into a PNG. |
| event `output_captured` | Where *that* picture went, or that it did not happen. |
| request `move_pointer` | Move the seat's pointer, as a mouse would. |
| request `pointer_button` | Press or release one of its buttons. |
| request `scroll_pointer` | Scroll where it is, as a wheel or a touchpad would. |
| request `keyboard_key` | Press or release a key on the seat's keyboard — the arrows, not text. |
| event `output_app_id` | What the application on one display *is*, as opposed to what its title says. |
| request `set_output_hdr` | Drive one display in high dynamic range, and say how bright and how saturated. |
| event `output_hdr` | Whether a display can be driven in HDR, whether it is, and the peak it reports. |
| event `output_hdr_controls` | Which of the HDR settings that display can actually honour. |
| event `output_mode` | One mode a display can be driven at, with whether it is the current one and whether the display names it as its own. A batch of them ends in `output_modes_done`. |
| request `set_output_mode` | Drive one display at a different resolution and refresh rate. |
| event `output_transform` | Which way up a display's picture is drawn — and, by being sent at all, that this compositor is the one turning it. |
| request `set_output_transform` | Turn one display's picture, for a screen standing on its side. |
| request `hide_pointer` | Take the cursor off screen, because the user has picked up the controller. |
| request `ask_to_share` | The desktop portal asking whether an application may see a display. |
| event `share_request` | That question, on its way to the shell — the only client that can draw it. |
| request `answer_share` | The shell's answer: a display, or nothing, which is a no. |
| event `share_answered` | That answer, on its way back to whoever asked. |
| request `offer_kind` | One kind of file an application will accept, ahead of the question that names it. Repeated once per pattern. |
| event `pick_kind` | That kind, on its way to the shell, in the order it was offered. |
| request `ask_to_pick_files` | The desktop portal asking for a file, some files, a folder, or somewhere to write. |
| event `pick_request` | That question, on its way to the shell — the only client that can draw it. |
| request `chose_file` | One file the user picked, repeated once each. |
| request `answer_pick` | The end of the question: whatever was named before it, and which kind was in force. Nothing named at all is a cancellation. |
| event `pick_chosen` / `pick_answered` | Those, on their way back to whoever asked. |
| request `cover_output_in_black` | Fade one display to black, or bring it back — the sheet [OLED protection](#oled-protection) rests a screen behind. It takes no input away, unlike the curtain the session goes out behind. |
| event `output_in_use` | Whether an application is in front of one display — anything the user opened, whatever started it — as against the shell being the whole of what is on the screen. |
| event `output_drawing` | Whether anything on one display has painted recently — which is what tells a screen that can be rested from one somebody is watching. |
| event `output_pointer` | The pointer is moving over this display. Sent on arrival and at most once every two seconds after, because the shell sees the pointer only where its own surfaces are in front. |
| request `set_picture_in_picture` | Whether a browser's picture-in-picture window floats over everything, how large it is drawn and which corner it sits in. One request for all three, because they are one rectangle. |
| request `set_window_floating` | The user's own word for whether one window floats, whatever it calls itself: a video told to fill the display it is in the corner of, and an application told to go and sit in that corner. One request in both directions. |
| request `set_menu_surface` | Which surface of the shell's a context menu is being drawn on, so that one surface can go in front of a floating window. Said only while a menu is up, and null again after. |
| request `ask_for_the_picture_behind` | Hands over a shared-memory buffer and asks the compositor to draw into it what it is compositing on one side of the shell's own surfaces — for the glass on them to refract. One ask, one picture. |
| event `the_picture_behind` | That buffer now holds it, and how much of it was drawn into. A size of zero means there was nothing to draw. |

`output_foreground` is what lets the menu say *Close KWrite* and notice when an
application it started has exited. It is reported per display because the
overlay belongs to one display: labelling it from the session's topmost window
offers to resume, or close, something on a screen the user is not looking at.

A window is attributed to the display it covers most of, rather than every
display it touches — otherwise one spilling over an edge claims both.

`capture_window` is a request because a Wayland client cannot photograph
another client's window, and the shell is a client: what it holds is a layer
surface of its own, so it cannot read even the application it is drawn over.
Only the compositor has those pixels. Where the file goes is the shell's to
decide, though, and is passed in — that is a question about the user's home
directory rather than about the display server — and `window_captured` always
answers, because a shell that has told the user it took a screenshot has to be
able to say where it went or that it did not happen.

`capture_output` is the same trade for a whole screen, and answers a different
question: not "what does this application look like" but "what is on this
display", which is a picture of the composite — wallpaper, windows, and the
shell's own bar or overlay over them, in the order they are drawn. It is taken
at as many pixels as the display is driven at and turned the way the display is
turned, and the pointer is left out of it: the cursor is drawn by the
compositor rather than being part of any surface, it is not on screen at all
while the session is driven from a controller, and an arrow burnt into a
screenshot cannot be taken back out.

`screenshot` is the round trip that makes a screenshot *key* possible. The
binding has to be the compositor's — the picture is of whatever is in front, so
a key that only worked while the shell had focus would never work over the game
somebody wanted a picture of — and the folder has to be the shell's, because
which folder that is depends on the language the account was made in. So the
key comes down as an event naming the display it was pressed on, and the path
goes back up as `capture_output`. The display then **flashes**, once, and only
after the picture has actually been written: it is the whole answer the user
gets, because a panel would be drawn behind the fullscreen application it was
reporting on, or in front of it with the keys taken off what they were doing.

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

It is session-private and, unlike wlr-layer-shell, is not offered to ordinary
clients at all — see above. The shell degrades cleanly without it —
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
| Clicking on another display | Hands control to that display, and nothing more |

Control is taken, never wandered into. A pointer resting on the other screen
changes nothing there: it is answered by the shape of the cursor and by nothing
else, and the display holding control goes on holding it until a click, a
shoulder button or `Tab` says otherwise. Control used to follow the pointer
across, which made a mouse crossing a screen edge — on the way to somewhere
else — enough to move the guide, the keyboard and the next launch onto a
display the user was not using. The first click on a display without control is
spent taking it, for the same reason: what that click would otherwise press was
chosen while another screen was being driven, and the overlay it appears to
land on was somewhere else when the button went down. The wheel is the one
exception, because turning it *is* an act on the display under it: it takes
control and then turns what it has taken.

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

## Shell audio

Every move that lands somewhere clicks, and a move that lands nowhere does not.
Pressing an edge is answered by nothing happening, which is why nothing sounds
there either. Which control made the move is not part of it: a click or a
finger on a row of the bar puts the selection there exactly as a direction
does, and the same move made by hand is owed the same answer.

The shell's two screens each have a voice, and each answers a move and a press
in it. The start screen moves with `press.ogg` and takes a press with
`press-selected.ogg` — a subcategory opened, a setting chosen, a search field
raised. The Home Button guide moves with `press-guide.ogg` and takes a press
with `press-guide-selected.ogg`. They are separate because the guide is a
screen of its own rather than another column of the start screen, and a user
who has looked away should be able to hear which of the two they are
driving.

Panels are not screens and do not follow the one they were opened over. A
context menu, a centred dialog and the mixer keep the same voice wherever they
were raised, because a component that changed its sound with its backdrop would
be two controls that look alike. Raising the guide from an otherwise empty
start screen also leaves its background music playing; raising it over an
application does not start that music above the application.

Leaving a lattice subcategory is `press-back.ogg`, whether Left or Back walks out
of it or a pointer or finger presses the visible trail or category row. One
gesture makes one sound even when a trail press crosses several levels: the
sound answers the decision to go back, not every column it passes.

A key of the on-screen keyboard going down is `keyboard-click.ogg` rather than
either screen's click, because the board is its own instrument: putting a key
*down* is the thing that only happens there, while walking across its keys is
walking a bar and still sounds like one. Every key of it, including Shift and
the key that puts the board away, because a board where two of the keys
answered silently would read as a board with two dead keys on it.

An application starting from the start screen is `app-launch.ogg`, and it is
the one sound here that is not a click, because it is not an acknowledgement:
the press has already been answered by the splash growing out of the tile, and
what this one says is that something is on its way. It belongs to that screen,
and only to it: a tile pressed on the start screen sounds it whether the shell
forks for it or comes back to a program that is already up, because the tile is
where an application is *started* from. The guide never sounds it. Every press made on the overlay
is answered in the overlay's own voice, the ones that hand an application the
display included — Resume with something running behind it, and a window card
chosen out of the deck, both `press-guide-selected.ogg`. A screen with a voice
of its own does not borrow another's for half of its rows. A press that failed
to start anything sounds nothing at all.

`guide-open.ogg` belongs to a *button* rather than to a screen, and it is the
only one that does. The guide button works from inside anything — a game holding
every other key, a panel, the on-screen keyboard — so it is the one press whose
answer cannot be "look at the screen and see": it may have been pressed exactly
because what is on the screen has stopped listening. It sounds when that press
*opens* the overlay and at no other time. Closing it is silent, because the
overlay leaving is the screen behind it coming back. So is the guide arriving by
any other route — Back walking out of the top of the bar, a portal's question
about sharing a screen, an authorisation panel raised over a game — because none
of those is somebody asking for the guide.

`screenshot.ogg` is one of the two sounds here that answer something the shell
*did* rather than something the user pressed. It is the pair of the white flash the
compositor draws over a display that has just been [photographed](#screenshots),
and it sounds at the same moment for the same reason: the chord is often spelled
with the user's eyes on the game, and an acknowledgement only one sense can
reach is one half of an acknowledgement. Like the flash it waits for the file to
be on the disk, so it says a picture exists rather than that a chord was
spelled, and a capture that failed sounds nothing. The picture of a single
window does not take it either — that one is answered by a panel naming the
folder, on a screen the user is already looking at.

`polkit.ogg` is the other, and it answers the one panel in the shell that nobody
asked for: an [authorisation prompt](#authorisation-prompts), raised over
whatever was in front of the user because a program somewhere wants a password.
Everything else the shell says is a reply to a control that was just pressed;
this one has to announce itself, or a question that has taken the screen arrives
in silence for anybody who happened to be looking at the room. It sounds as the
panel goes up and only if it went up, and a password refused does not sound it
again — that is the same question still waiting, not a new one.

The start screen has the eleventh recording, `start-bg-music.ogg`. It belongs
to the session rather than to a screen: it loops while every display is showing
the start screen and nothing at all is open, and it fades as a launch or a
returning window begins taking any of them. One application anywhere ends it —
a game on the first display and the start screen on the second is a session
with a game in it, and crossing to that second screen must not start music up
behind the game. When the
last application closes, the shell constructs a fresh stream at sample zero —
even if the previous one is still fading — so coming back never resumes halfway
through the track.

It is also the one recording that can be turned off outright, from [`Settings >
Sounds > Start music`](#sounds), which is where the rest of that is written down.
The ten short clips cannot: each of them answers a control that was pressed or an
event that arrived, and a press with no answer reads as a button that does not
work. A background is the sound the shell makes at somebody who has pressed
nothing, which is exactly what a person reading in the same room may not want.
Turned off it stops the way muting stops it — at once, and without the fade an
application gets — and turned back on it begins again from the beginning.

All eleven recordings are shipped in the repository beside the shell's source
for the same reason the fonts and the cursor are: there may be no desktop on
the machine, and so no theme of sounds to borrow one from. The ten short
effects are decoded once at start-up, and every press is its own sound rather
than one restarted, so a held D-pad walks the bar to a run of clicks rather
than to one click held at the point the repeats overtook it. The much longer
music stays compressed and is decoded as it plays instead of occupying a
whole decoded track's worth of memory.

What no press does is lay a sound on top of a copy of itself. Identical
recordings add, and being identical they add in phase, so a spun wheel used to
answer with one click many times its own height: a single scroll event can be
worth several rows, every row it crosses clicks, and a dozen arriving together
took the output to full scale. A click asked for within a sixteenth of a second
of the last of its kind is dropped rather than mixed. Two clicks that close are
not two sounds anyone can hear apart, and the gap is shorter than the repeat of
a held direction, so a run of clicks is still a run of clicks.

The `System` row of the volume mixer in the guide sets how loud all of this is —
the shell's effects and background music, and the one row on that panel no
sound server knows about. Left and Right move it, `A` silences it, and the
click the direction makes is heard at the level it has just been moved to, so
the row previews itself. Muting also drops the music rather than advancing it
silently; unmuting on the start screen begins it again. The setting is written
to
`shell.toml` as `sound-volume` and `sound-muted` at every step rather than when
the user stops moving it: the shell is idle between presses, and a level nobody
wrote down is the one a machine switched off at the wall would lose.

Playing is in-process, unlike everything else the shell does with audio. The
volume bars in the guide shell out to `wpctl`, `pactl` or `amixer`, because
what they set is what the *session* is doing; shell audio is one more stream
playing into that session, its effects have to start within a frame of the
button, and a process spawned per step would be a fork every 90 ms under a held
direction.
The output is opened through ALSA, which is PipeWire or PulseAudio on a machine
that has one and the sound card itself on a machine that does not. A session
whose sound server is not up yet is retried a few seconds later; if an open
output disappears, the next loop drops the dead stream and opens the current
default. Start music is rebuilt there from sample zero if the lattice still owns
the display. A machine with no output at all is silent, and nothing else about
the shell changes.

## What a pane of glass shows

Every pane in this shell refracts what is behind it, and "behind it" is three
different things it reaches three different ways.

What the **shell itself** drew is read straight back: the frame is built in an
offscreen texture and a pane samples a snapshot of it, so a button refracts the
panel it is resting on and a panel refracts the icons under it. What the shell
draws on *another* of its surfaces is read the same way, at full resolution —
that is how a context menu, which has a surface to itself, still refracts the
start screen behind it.

The **wallpaper** is not read at all: it is *evaluated*, from the same function
that paints it, wherever the snapshot is transparent. That is sharp at any size
and costs nothing, and it is why a game's key art — which is drawn as the
wallpaper rather than as a layer over it — refracts correctly too.

What **another client** drew, the shell can do neither with. A game's window, or
the video in a floating one, is the compositor's pixels and this shell never sees
them. So the compositor draws them and hands them over: `lxb_shell_v1`'s
`ask_for_the_picture_behind` gives it a shared-memory buffer, it draws what it is
compositing on one side of the shell's own surfaces into it, small, and says when
it is done. One ask, one picture — a display whose shell is not drawing asks for
nothing, so a game in front pays for none of this.

Small on purpose. A pane frosts what it transmits — it samples several rungs down
a blur chain — so what it wants back is something already blurred, and 256 pixels
along the longer edge is a readback a shell can afford every frame. It is one
frame behind, which is 16 ms of a picture about to be frosted past recognition.

And it is **absorbed on the way in**. Everything here is designed against a
wallpaper that is deliberately dark, which is what makes a pane's tint thin
enough to be worth seeing through. A film or a game let through at full strength
turns the same pane into a window: over a bright frame its own colour disappears
and the white text on it stops being readable, which on a menu is a control that
cannot be answered. Tinted glass absorbs what it transmits, and it absorbs the
same amount whatever is behind it, so the material reads the same everywhere.

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

The shell starts drawing again for the four things it puts in front of an
application: a launch splash, the on-screen keyboard, a bubble in the corner
and the control a volume key raises. It then draws *only* that thing — the
start screen belongs behind the application, and a shell that drew both would
lay every icon of the bar over the game underneath.

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
| `Print` (with anything held), `Ctrl+Shift+3`, `Alt+Shift+3` | Photograph this display |
| `XF86AudioRaiseVolume` / `XF86AudioLowerVolume` / `XF86AudioMute` (with anything held) | Turn the session up or down, or silence it |
| `Super+Tab`            | Cycle windows on this output    |
| `Alt+Tab`, `Alt+Shift+Tab` | Walk the [guide's deck](#the-cards) while the modifier is held, and take what it lands on |
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
  config.rs       config.toml, and the defaults a missing one gives
  outputs.rs      multi-display layout: positions, scale, transform, tiling
  input.rs        input routing, focus policy, keybindings
  focus.rs        common Wayland/X11 keyboard, pointer and touch targets
  cursor.rs       the compositor-drawn cursor and its XCursor theme
  text_input.rs   text-input, input-method and virtual-keyboard, for the
                  on-screen keyboard
  shell_control.rs  compositor half of lxb_shell_v1
  overview.rs     the windows of a display animated into the guide's cards
  restore.rs      one window flown back out of the tile that asked for it
  teardown.rs     ending an application, as against ending a process
  capture.rs      photographing one window, or one whole display, into a PNG
  flash.rs        the white a display gives when it has just been photographed
  blackout.rs     the black one display rests behind while another one is
                  being used — see OLED protection
  screencopy.rs   wlr-screencopy: the standard way anything else reads the
                  screen, and what the portal is built on
  hdr.rs          the connector's metadata and the CRTC's colour pipeline
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
  appinfo.rs      what installed an application, and what that says about it
  uninstall.rs    one Origin translated into one argv, and whether it may run
  steam.rs        Steam as the shell holds it: one account, one sign-in
                  panel, one column
  polkit.rs       the session's polkit agent: polkitd on one side, PAM's
                  helper on the other, and the panel in between
  secret.rs       a password, from the key that types it to the pipe that
                  consumes it
  icons.rs        icon theme lookup, PNG/SVG rasterisation
  theme.rs        the palette: twelve accents, and every colour read as it is drawn
  settings.rs     the Settings column, written here rather than found on disk
  model.rs        the shared catalogue, and one cursor per display
  controller.rs   gamepads through gilrs, and what a button means
  pad_guard.rs    the pad taken away and given back with the guide button
                  missing, so that no application can read it
  steam_hid.rs    the second-generation Steam Controller, read from hidraw
  guide.rs        the overlay's modes and menu
  menu.rs         the context menu: entries, selection, scrolling and
                  growth, with no idea what raised it
  dialog.rs       the centred panel: a question that has taken the screen over
  launch.rs       the splash between pressing A and the application being there
  media.rs        the walk over $HOME for music, films and photographs,
                  what order the rows are in, and what opens one
  thumbs.rs       a frame of the film, a photograph scaled down, and the
                  freedesktop cache both are kept in
  trash.rs        the freedesktop trash, for the Delete row
  transfer.rs     the folder a file is carried to, walked as a mirrored bar,
                  and the copy or move itself; a name is changed in main.rs,
                  beside it, because it is one `rename` and a field
  screenshot.rs   where a screenshot goes, in the language the account was made in
  pointer.rs      the right stick as a mouse, and which applications it is
                  turned on for
  keyboard.rs     the on-screen keyboard: its keys, the input method and
                  virtual keyboard behind them, and the grab that lets a
                  real keyboard drive it
  system.rs       volume, per-application volume and brightness, off the
                  main thread
  power.rs        what is left in the battery, out of the kernel's own
                  power_supply directory, and which supplies are this
                  machine's rather than a peripheral's
  volume.rs       how long the control a volume key raises stays on screen
  sound.rs        the ten effects and Start music, and the output and focus
                  transitions they go through
  ui.rs           layout: model to quads and text runs
  gpu.rs          wgpu renderer, one atlas and two pipelines
  shaders.wgsl    animated backdrop, instanced quads
  offscreen.wgsl  the blur the glass reads through, and the copy to the display

crates/lxb-steam/
  lib.rs          the worker thread, and the two channels the shell holds
  auth.rs         signing in: IAuthenticationService, both ways round
  library.rs      what the account owns, and what of it is on this disk
  client.rs       Valve's client as a background process: where it is,
                  whether it is up, whether it has signed in
  webui.rs        the calls this shell makes into the client's own
                  interface — signing it in, and moving a game on or off
                  the disk — and why they are made there
  session.rs      what survives a reboot, and what must never be written down
  protobuf.rs     the wire format Steam's services speak, written out by hand
  vdf.rs          Valve's key-values, which is what the disk answers in
  rsa.rs          encrypting the password under the account's own key
  password.rs     it, from the shell's field to that encryption
  base64.rs       the two spellings Steam uses
  web.rs          one HTTPS agent, and the two shapes of call made over it
  qr.rs           the code on the screen, as squares for the shell to draw

crates/lxb-portal/
  cast.rs         one display, going out as a PipeWire stream
  screencast.rs   org.freedesktop.impl.portal.ScreenCast, over D-Bus
  consent.rs      who may see the screen, and which one
```

## Not implemented

Worth knowing before you rely on this:

- **A see-through overlay without a blendable surface.** The overlay asks for
  a premultiplied-alpha surface so the running application shows through it.
  A driver offering only `Opaque` gets a working menu on a solid background
  instead, and says so in the log.
- **Sharing one window rather than a whole display.** The portal offers
  displays only. A window's pixels can be photographed (`capture_window`) but
  not streamed, and `AvailableSourceTypes` says so rather than offering it and
  failing.
- **Gesture navigation in the shell.** [Mouse and touch](#mouse-and-touch)
  answers clicks, taps and the wheel; there is no swipe, pinch or two-finger
  handler on the bar. The compositor forwards pointer gestures to clients
  normally.
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
- **Signing Valve's client out.** The shell signs it *in* by calling the method
  the client's own login screen calls. There is no matching call for signing
  out: the client's own is `SignOutAndRestart`, and the restart puts its login
  window on the screen, which is the one thing this must never do. So signing
  out of the shell stops the client and clears the two files that would sign it
  back in, but leaves Valve's own cached credential alone — guessing at an
  unpublished format is how somebody's Steam configuration gets corrupted.
  Someone who then starts Steam **by hand** may find it still signed in, and
  signs out from inside it as they always would. The row is called **Sign out
  of LineXinBar** for that reason, and the panel behind it says so and offers
  to open Steam to finish the job.
- **Playing without Valve's client.** There was a version of this that started
  games itself, answered their Steamworks calls with its own library and fetched
  content out of Valve's depots. It worked, and it broke on every game that did
  anything unusual — a loader that opens `libsteam_api.so` by name, a Windows
  game whose prefix Steam had already made, an anti-cheat that wants the real
  client's pipe. It is gone. A machine with no Steam client can sign in and see
  its library, and can start nothing in it.
## License

GNU General Public License v3.0 only. See [LICENSE](LICENSE).
