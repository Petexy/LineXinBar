# LineXinBar packaging

These definitions build three early-development packages from one source tree:

* **`lxb-compositor`** — the compositor and the cursor theme it draws the
  pointer from. A Wayland session on DRM/KMS, with nothing in it that assumes
  this project's shell is the one running. This is what a display manager
  depends on to put a login screen on the hardware without installing a
  desktop, and Console Experience Desktop Manager is the one that does.
* **`lxb-desktop`** — the shell, the portal and the Wayland session entry. It
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

The split is a partition, and `packaging/build.sh check` enforces that — a file
installed by no package is one that has quietly stopped shipping, and a file
installed by two is one two packages will fight over at install time.

Between them the three packages install the compositor and shell, a complete
bundled cursor theme, a native Wayland session, and the optional integration:

```text
lxb-compositor   bin/lxb
                 share/icons/Bibata-Modern-Classic/**

lxb-desktop      bin/lxb-desktop
                 bin/lxb-portal
                 bin/lxb-session
                 share/wayland-sessions/lxb.desktop
                 share/xdg-desktop-portal/**
                 share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service

lxb-retroarch    bin/lxb-retroarch
                 share/lxb/glyphs/retroarch.svg
                 share/lxb/glyphs/console-*.svg   (one per console it knows)
```

`lxb-session` clears display variables inherited from a greeter, creates a
private D-Bus session, and starts `lxb --backend udev --shell`. This gives a
display manager one foreground process whose lifetime is tied to the desktop
shell. The original application sources and `share/wayland-sessions/lxb.desktop`
are not modified; packages stage the production session files from
`packaging/files/`.

## One version, in one file

The version this project releases under is the single line in `VERSION` at the
root of the checkout — **0.9.0** — and what a package claims and what
`lxb --version` reports are the same number because both come from there.

Almost everything reads that file where it stands: the Arch, Debian and Nix
definitions, the source archive's name, and the build scripts in
`crates/lxb-compositor`, `crates/lxb-desktop` and `crates/lxb-retroarch`, which
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
stages the common payload, validates the standard desktop-entry fields, and
verifies that all cursor-theme symlinks survive. Use `--no-build` only when
current release binaries already exist in `target/release` (or in
`$CARGO_TARGET_DIR/release`, which the staging step follows).

The locked Rust dependency graph currently requires Rust 1.89 or newer, even
though the workspace's older `rust-version` declaration has not been changed.

## Debian

Build on Debian, Ubuntu, or another Debian-derived system with the development
packages listed in the main README plus `dpkg-dev`:

```sh
./packaging/build.sh debian
```

The builder uses `dpkg-shlibdeps` on the locally linked binaries, stages a
policy-shaped binary package, and writes it to `packaging/out/debian/`.
Wayland, EGL and X11 libraries that the application opens dynamically are
declared explicitly because ELF dependency scanning cannot see them.

`--allow-foreign-host` exists for package-structure testing only. A `.deb`
built against another distribution's libc and libraries must not be deployed
on Debian.

## Fedora

Build on Fedora after installing RPM build tooling and the `BuildRequires`
listed in `packaging/fedora/lxb-desktop.spec`:

```sh
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
so neither needs a system library. Package scripts do not create users, change group
membership, install a system-wide configuration, or alter device permissions.

These recipes are intended for local and CI packages during early development.
Before submission to an official distribution archive, build in that
distribution's clean builder and complete its dependency-license review.
