# LineXinBar packaging

[Documentation](index.md) · [Project home](../README.md)

Run the commands in this guide from the repository root.

These definitions build three early-development packages from one source tree:

* **`lxb-compositor`** — the compositor and the cursor theme it draws the
  pointer from. A Wayland session on DRM/KMS, with nothing in it that assumes
  this project's shell is the one running. This is what a display manager
  depends on to put a login screen on the hardware without installing a
  desktop, and Console Experience Desktop Manager is the one that does.
* **`lxb-desktop`** — the shell, the portal, the update coordinator and the Wayland session entry. It
  depends on the exact `lxb-compositor` built beside it, version and release
  both: two halves of one build that drift apart are a shell talking to a
  compositor it was never tested against.
* **`lxb-retroarch`** — the optional RetroArch integration: one helper program
  and the two marks the shell draws its rows with. Optional in the other
  direction from the compositor: the shell looks for `lxb-retroarch` on `PATH`
  and mentions RetroArch only when it is there, so a machine that will never
  emulate a console carries none of it. Version-locked to the exact
  `lxb-desktop` beside it, because what the two agree about is a protocol
  carried on a pipe rather than a library — see `crates/lxb-retroarch`.
* **`lxb-heroic`** — the optional Epic Games integration, through Heroic Games
  Launcher's flatpak: one helper program and the mark the shell draws its rows
  with. Optional on exactly the terms `lxb-retroarch` is, and version-locked to
  the `lxb-desktop` beside it for the same reason. It depends on `flatpak`,
  which it cannot do without — see [Epic Games](epic.md) and
  `crates/lxb-heroic`.

The split is a partition, and `packaging/build.sh check` enforces that — a file
installed by no package is one that has quietly stopped shipping, and a file
installed by two is one two packages will fight over at install time.

Between them the four packages install the compositor and shell, a complete
bundled cursor theme, a native Wayland session, and the optional integrations:

```text
lxb-compositor   bin/lxb
                 share/icons/Bibata-Modern-Classic/**

lxb-desktop      bin/lxb-desktop
                 bin/lxb-portal
                 bin/lxb-updates
                 share/doc/lxb-desktop/updates.md
                 bin/lxb-session
                 share/wayland-sessions/lxb.desktop
                 share/xdg-desktop-portal/**
                 share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service
                 lib/udev/rules.d/70-linexinbar-input.rules

lxb-retroarch    bin/lxb-retroarch
                 share/lxb/glyphs/retroarch.svg
                 share/lxb/glyphs/console-*.svg   (one per console it knows)

lxb-heroic       bin/lxb-heroic
                 share/lxb/glyphs/epic.svg
```

The udev rule grants two device nodes to whoever holds the active session on
the seat — `uaccess`, not a group, so it is the person sitting there and never
every account on the machine. They are the only two the shell opens itself: the
second-generation Steam Controller's `hidraw`, which no kernel driver claims
and which is therefore `0600 root:root` with nothing in systemd's own rules
tagging it, and `/dev/uinput`, which is how a guarded pad and a stand-in gamepad
are handed back to the rest of the machine. Without the rule the controller
handling half works and says nothing about why, which is why it ships here
rather than being left to whatever device-rules package a distribution happens
to have.

That narrows which *account* holds the nodes and not which program, and the
difference matters here: every process of that account is inside the ACL,
including Valve's client and every game it launches. `/dev/uinput` is input
injection — anything running as the user can publish a virtual keyboard and
synthesise presses the compositor cannot distinguish from the real one's — and
`uaccess` grants `rw` on the pad's `hidraw` whatever `MODE=` says, though the
shell only reads it. This is a wider trust boundary than the rest of the
session's, which refuses `lxb_shell_v1` to exactly those programs, and closing
it needs a root-owned helper that hands out descriptors rather than a rule that
hands out nodes. There is none yet. The [input permissions guide](getting-started.md#what-the-input-rule-actually-opens)
sets out the whole of it; packagers shipping this rule are shipping that.

`lxb-session` clears display variables inherited from a greeter, creates a
private D-Bus session, and starts `lxb --backend udev --shell`. This gives a
display manager one foreground process whose lifetime is tied to the desktop
shell. The original application sources and `share/wayland-sessions/lxb.desktop`
are not modified; packages stage the production session files from
`packaging/files/`.

## Update dependencies

The desktop package requires **fwupd** and **polkit/pkexec** on Arch, Debian,
Fedora and Nix. Arch also requires pacman-contrib for safe update checks. Keep
these dependencies in ports to other distributions. NixOS enables the fwupd
service through the module. Optional Flatpak, Snap and AUR tools are detected;
installing the desktop does not install every package manager.

The coordinator ships with the desktop and has no blanket root authorization
policy. Install the helper as root-owned in a protected executable path; a
user-writable development binary cannot obtain a job authorization. A working
logind/elogind service is required for protected installation; updates fail before
mutation when an inhibitor cannot be acquired. It uses a transient systemd user
service where available, with a detached
fallback. Ports with other service managers should validate session teardown and
provide supervision before promising updates survive logout. See
[the update guide](updates.md) for policy examples and the validation matrix.

A distribution that carries these packages, and the six projects released beside
them, has nothing to configure for **Update LineXinBar**. The row appears only
when some of the family is installed as packages none of the system's
repositories offer, or was built from source under `/usr`, `/usr/local` or
`~/.local`. The coordinator then reads each project's release list from
`api.github.com` and downloads release files from `github.com`, over a TLS stack
and root certificates of its own. Building a tag from source also needs `git`
and a system-wide Rust toolchain, which are not dependencies of the package:
only a machine that installed from source needs them, and it has them already.
See [LineXinBar and the projects beside it](updates.md#linexinbar-and-the-projects-beside-it).

Immutable distributions can package a custom System updater without enabling a
native system upgrade. Ship the provider selection in
`/usr/share/linexinbar/updates.json`, its manifest under
`/usr/share/linexinbar/update-providers.d/`, and the declared executable with its
dependencies. See [Custom system update providers](updates.md#custom-system-update-providers)
for file modes, administrator overrides, the check/apply JSON contract and a
Bash adapter example. These are distribution-owned additions, not user-editable
Settings commands.

## One version, in one file

The version this project releases under is the single line in `VERSION` at the
root of the checkout — **0.9.1** — and what a package claims and what
`lxb --version` reports are the same number because both come from there.

Almost everything reads that file where it stands: the Arch, Debian and Nix
definitions, the source archive's name, and the build scripts in
`crates/lxb-compositor`, `crates/lxb-desktop`, `crates/lxb-retroarch` and
`crates/lxb-heroic`, which
refuse to build a binary whose manifest has drifted away from it. The last of
those needs the check most: it is the one binary here that can be installed
without the other two, so it is the one that can most easily be a version out of
step with what it is talking to. Two places cannot read a file
and carry the number as a literal instead — `[workspace.package]` in
`Cargo.toml`, which is where `--version` gets it, and `Version:` in the Fedora
spec, which has to be a literal for the spec to be one anyone could submit — so
a release is one command that writes those from the file:

```sh
./scripts/bump-version.sh 0.2.0
```

That writes `VERSION`, the manifest, `Cargo.lock` (every packaged build is
`--locked` or `--frozen`, so a lock left behind is a build that refuses to
start) and the spec's `Version:`, and then asks for the one thing only a person
can write: a `%changelog` entry. `packaging/build.sh check` refuses to let any
of them drift apart.

## Where the build happens, and why not /tmp

Every builder works under `packaging/out/build/`, on whatever filesystem the
checkout is on. Not `${TMPDIR:-/tmp}`, which is the obvious choice and the wrong
one: on a systemd machine /tmp is a tmpfs sized at a fraction of RAM, so
building there means building in memory. This dependency graph — smithay, wgpu,
naga, winit, pipewire — writes about 1.7 GiB compiling in the release profile,
and the `cargo test` that makepkg's `check()` and rpmbuild's `%check` run builds
the whole of it again in the dev profile for roughly 5 GiB more. That is 6.7 GiB
of writes into a filesystem sized as a fraction of RAM, and a tmpfs that is
already carrying anything else runs out partway through. It reports that as `No
space left on device` — or, where the tmpfs carries quotas, as `Disk quota
exceeded (os error 122)`.

Send it elsewhere with `--work-dir DIR` on the Arch and Fedora builders, or
`LXB_WORK_DIR` for all of them:

```sh
./packaging/build.sh arch --work-dir /var/tmp/linexinbar
LXB_WORK_DIR=/var/tmp/linexinbar ./packaging/build.sh fedora
```

A work directory inside the checkout is refused unless it is under
`packaging/out`, because `snapshot_source` picks up untracked files and a build
tree anywhere else would end up inside the source archive built from it.

The builders check free space before extracting anything, so a machine without
the room is told immediately rather than forty minutes in. That check reads
`df`, which cannot see a quota — the default location is what actually solves
the quota case. One thing it cannot route around either: `makepkg.conf` wins
over the environment, so a machine that sets `BUILDDIR` builds there whatever
`--work-dir` said. The Arch builder notices and says so.

## Validate the shared payload

```sh
./packaging/build.sh check
```

This checks shell/package syntax, confirms every package definition still
takes its version from `VERSION`, builds the release binaries,
stages the common payload, validates the standard desktop-entry fields,
verifies that all cursor-theme symlinks survive, and checks that the input
device rules still grant what they are for — `udevadm verify` reads them where
it is installed.

It also runs the device tests and says what they could actually reach on the
machine the release is being built on. Ten of the shell's tests need hardware,
and each of them prints a line and passes where there is none; `cargo test`
counts that as a pass, so a release built without a controller used to report
full coverage of the controller work and have none of it. The report names every
test that was not covered and why. A skip is never fatal — it is a fact about
the build machine, and one worth writing into a release note. Run it on its own
with `./packaging/device-report.sh`. Use `--no-build` only when
current release binaries already exist in `target/release` (or in
`$CARGO_TARGET_DIR/release`, which the staging step follows).

The locked Rust dependency graph currently requires Rust 1.89 or newer, even
though the workspace's older `rust-version` declaration has not been changed.

## Debian

Build on Debian, Ubuntu, or another Debian-derived system with the development
packages listed in [Getting started](getting-started.md#building) plus `dpkg-dev`
and the PipeWire headers. On Debian 13 take Rust from `rustup` rather than
`cargo`: the distribution's own is 1.85, older than the locked dependency graph
allows.

```sh
sudo apt install dpkg-dev build-essential pkg-config clang rustup \
    libasound2-dev libgbm-dev libavcodec-dev libavformat-dev libavutil-dev \
    libswscale-dev libinput-dev libpipewire-0.3-dev libseat-dev libudev-dev \
    libxkbcommon-dev
./packaging/build.sh debian
```

That is what the build itself links; the builder checks for all of it before
compiling anything and, if something is missing, names the packages in one
`apt install` line. A distrobox or toolbox container on a plain `debian` image
is enough, and there `rustup` finds the toolchain already in the shared
`~/.rustup`.

The builder uses `dpkg-shlibdeps` on the locally linked binaries, stages a
policy-shaped binary package, and writes it to `packaging/out/debian/`. It
compiles into `target/debian` rather than `target/` (or into
`$CARGO_TARGET_DIR` when that is set), so a build made in a container that
shares the checkout never replaces the host's own binaries.
Wayland, EGL and X11 libraries that the application opens dynamically are
declared explicitly because ELF dependency scanning cannot see them.

`--allow-foreign-host` exists for package-structure testing only. A `.deb`
built against another distribution's libc and libraries must not be deployed
on Debian.

## Fedora

Build on Fedora after installing RPM build tooling and the `BuildRequires`
listed in `packaging/fedora/lxb-desktop.spec`, which `dnf builddep` reads from
the spec itself. A distrobox or toolbox container on the `fedora-toolbox` image
is enough:

```sh
sudo dnf install rpm-build dnf5-plugins git-core
sudo dnf builddep packaging/fedora/lxb-desktop.spec
./packaging/build.sh fedora
```

The builder snapshots the current working tree, runs `cargo vendor --locked`,
and creates an offline source archive before invoking `rpmbuild -ba`. Binary
and source RPMs are copied to `packaging/out/fedora/`. Pass `--no-check` to
skip package tests, or append rpmbuild options after `--`.

The spec disables the debuginfo subpackage, because Cargo's release profile
emits no DWARF for `find-debuginfo` to collect. Submitting to the Fedora
archive means reversing that: build with `-Cdebuginfo=2 -Cstrip=none` under
Fedora's own path remapping and drop `%global debug_package %{nil}`.

## Arch Linux

Build on Arch or an Arch derivative with `base-devel`, Cargo and the package's
development dependencies installed:

```sh
./packaging/build.sh arch
```

The wrapper creates a deterministic source archive and renders a PKGBUILD with
its real SHA-256 checksum before running `makepkg`. Artifacts are copied to
`packaging/out/arch/`. To let makepkg install missing dependencies, pass its
option explicitly:

```sh
./packaging/build.sh arch -- --syncdeps
```

## Nix and NixOS

The repository is a pinned flake and also exposes a channel-compatible
expression:

```sh
./packaging/build.sh nix
nix build path:.#linexinbar          # the whole desktop, as one derivation
nix build path:.#lxb-compositor      # the compositor alone
nix-build packaging/nix
```

Nix splits differently from the distro packages, and deliberately. Those split
to keep a dependency graph and a file list apart on an installed system, which
are problems Nix does not have; here `linexinbar` stays one self-contained
derivation and `lxb-compositor` is a second, smaller one for a consumer that
needs a Wayland session and not a shell. There is no shell-only derivation,
because one that had to find `lxb` in another store path would be strictly
worse than one that carries it.

The package adds runpaths for host GPU drivers and dynamically loaded
Wayland/EGL/Vulkan/X11 libraries, and carries its external session utilities
in the Nix closure. The result includes
`passthru.providedSessions = [ "lxb" ]`.

For NixOS, import the module and enable it; the module installs the package and
adds it to `services.displayManager.sessionPackages`:

```nix
{
  imports = [ inputs.linexinbar.nixosModules.default ];
  programs.linexinbar.enable = true;
}
```

## Runtime integrations

A hardware session needs systemd-logind or seatd and a working EGL/GLES GPU
driver. Xwayland, a Vulkan driver, `wpctl`/`pactl`/`amixer`, and `ddcutil` are
optional integrations. `lxb-retroarch` wants `flatpak` to be able to offer to
install RetroArch, and prefers a distribution package of RetroArch over the
flatpak where both are present; it needs neither to be installed to be
packaged. It reaches the network for two things and only when asked: the flatpak
install, and fetching a core from `buildbot.libretro.com` — the same server
RetroArch's own Online Updater uses. Both are HTTPS with a TLS stack of its own,
so neither needs a system library. `lxb-heroic` needs `flatpak`: everything it
does is Heroic's Flathub build, which it installs into the user's own flatpaks
where there is none. It reaches the network through that flatpak, through
Heroic's bundled legendary (Epic's own servers, on Heroic's session), and for
three things of its own: Epic's device sign-in, Heroic's default Proton from
GitHub (checked against its published checksum), and the covers, backdrops,
logos and achievement icons from Epic's image servers. Package scripts do not create users, change group
membership, install a system-wide configuration, or alter device permissions.

These recipes are intended for local and CI packages during early development.
Before submission to an official distribution archive, build in that
distribution's clean builder and complete its dependency-license review.
