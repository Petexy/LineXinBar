# Getting started

[Documentation](index.md) · [Project home](../README.md)

- [Requirements](#requirements)
- [Building](#building)
- [Running](#running)
- [Configuration](#configuration)

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
  everything else carries on. See [Display](settings.md#display).

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
| `libavcodec` / `libavformat` / `libavutil` / `libswscale` | A wallpaper of the user's own: [Custom wallpaper](settings.md#custom-wallpaper) decodes their picture or plays their film | shell |
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

See [Updates](updates.md) for provider coverage, administrator configuration,
recovery behavior and validation limits. Cross-distribution adapters are an
initial implementation; they still need testing on their target distributions.
Distributors building immutable systems can ship a
[custom System update provider](updates.md#custom-system-update-providers);
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
| `xdg-desktop-portal`                      | The front desk applications ask for a screen or for a file — [screen sharing](desktop-integration.md#screen-sharing) and [choosing a file](desktop-integration.md#choosing-a-file) both need this and `lxb-portal` | Applications find no portal: none can share a screen, and each falls back to whatever file dialog it has of its own |
| `steam` (native or Flatpak)               | Playing and installing anything in the Steam column | The account still signs in and the library is still listed, but nothing in it starts or downloads: every row says so rather than doing nothing |
| `lxb-retroarch` (a package of its own)    | [RetroArch and your own console games](retroarch.md#retroarch-and-your-own-console-games): a row under Steam, and a column of the consoles in your ROM folder | The shell never mentions RetroArch at all — no row, no column, no page under Settings |
| `flatpak`, with `lxb-retroarch` installed | Installing RetroArch from the shell, and running the Flathub build | The row says RetroArch is not installed and that there is no flatpak to install it with; a distribution package of `retroarch` is used in preference either way |
| `lxb-heroic` (a package of its own, needing `flatpak`) | [Epic Games](epic.md): a row under Steam, a sign-in by phone or on the screen, and a column of your Epic library to install and play | The shell never mentions Epic Games at all — no row, no column, no page under Settings |

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
- **`/dev/uinput`**, to keep [the guide button](controls.md#the-guide-button-is-the-shells-alone)
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
[`lxb_shell_v1`](architecture.md#lxb_shell_v1) is refused to all of it. These two nodes are the
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
  [`steam_hid`](../crates/lxb-desktop/src/steam_hid.rs) sets out at length why
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

A Rust toolchain **1.89 or newer**, plus `pkg-config` and the development
headers for the libraries above. The workspace still declares Rust 1.85 in
`rust-version`, but the locked dependency graph needs 1.89 or newer; see the
[packaging notes](packaging.md#validate-the-shared-payload).

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

Building the full workspace also needs **PipeWire development headers** for
`lxb-portal`: `libpipewire` on Arch, `libpipewire-0.3-dev` on Debian/Ubuntu,
or `pipewire-devel` on Fedora, in addition to the packages above. Run the
package installation commands with administrator privileges, and check that
your distribution’s Rust package meets the toolchain requirement.

Package names drift; the library list above is the thing to match if yours
disagrees. The shell also wants a Vulkan driver at runtime where one exists
(`vulkan-radeon`, `vulkan-intel`, `mesa-vulkan-drivers`), and falls back to GL
where it does not.

Everything built here reports one version, and it is the single line in
[`VERSION`](../VERSION) at the root of the checkout: every package definition
reads that file, and the compositor and the shell both refuse to build against
a manifest that has drifted away from it. `./scripts/bump-version.sh 0.2.0`
moves it, along with the two places that have to carry the number as a literal;
[the packaging guide](packaging.md) says which and why.

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

`--window-size` sets how big that window is, in pixels, and so what resolution
the session renders at. It means the same thing on both nested backends:

```sh
./target/release/lxb --backend winit --window-size 1920x1080
```

Without it the window is `1280x800`. The size is in real pixels rather than the
host's logical units, so a nested session is the resolution you asked for
whatever scale the desktop around it is set to.

### Nested with several virtual displays

The winit backend can only ever open one window. To exercise the
multi-display paths without owning extra monitors, the X11 backend opens one
window per virtual output (Xwayland is fine):

```sh
./target/release/lxb --backend x11 --outputs 3 --window-size 800x600
```

Each window is a real output with its own position in the logical layout,
which clients see through `wl_output` and `xdg-output`. `--outputs` is the one
flag here that is x11-only; the other backends say so rather than quietly
opening one window.

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
manager offer it, install [`share/wayland-sessions/lxb.desktop`](../share/wayland-sessions/lxb.desktop)
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

## Configuration

`$XDG_CONFIG_HOME/lxb/config.toml` (usually
`~/.config/lxb/config.toml`). Every field is optional; see
[`docs/configuration.md`](configuration.md) for the full reference and
[`examples/config.toml`](../examples/config.toml) for a commented sample.

