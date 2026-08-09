# LineXinBar packaging

These definitions build one early-development package named `linexinbar`. The
package version is **0.1.0**, which is the workspace's own version: what a
package claims and what `lxb --version` reports are the same number, and
`packaging/build.sh check` refuses to let the two drift apart. Bumping a
release means editing both `packaging/VERSION` and `[workspace.package]` in
`Cargo.toml`.

Every package installs the coupled compositor and shell, a complete bundled
cursor theme, and a native Wayland session:

```text
bin/lxb
bin/lxb-desktop
bin/lxb-session
share/wayland-sessions/lxb.desktop
share/icons/Bibata-Modern-Classic/**
```

`lxb-session` clears display variables inherited from a greeter, creates a
private D-Bus session, and starts `lxb --backend udev --shell`. This gives a
display manager one foreground process whose lifetime is tied to the desktop
shell. The original application sources and `share/wayland-sessions/lxb.desktop`
are not modified; packages stage the production session files from
`packaging/files/`.

## Validate the shared payload

```sh
./packaging/build.sh check
```

This checks shell/package syntax, confirms every package definition still
takes its version from `packaging/VERSION`, builds both release binaries,
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
listed in `packaging/fedora/linexinbar.spec`:

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
nix build path:.#linexinbar
nix-build packaging/nix
```

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
optional integrations. Package scripts do not create users, change group
membership, install a system-wide configuration, or alter device permissions.

These recipes are intended for local and CI packages during early development.
Before submission to an official distribution archive, build in that
distribution's clean builder and complete its dependency-license review.
