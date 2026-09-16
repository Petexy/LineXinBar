Name:           lxb-desktop
Version:        0.9.0
Release:        1%{?dist}
Summary:        Multi-display Wayland desktop with a console-style shell

# LineXinBar/Bibata, embedded Roboto and Roboto Mono, and the locked
# statically linked Rust dependency graph for Linux.
License:        GPL-3.0-only AND Apache-2.0 AND OFL-1.1 AND MIT AND BSD-2-Clause AND BSD-3-Clause AND ISC AND MPL-2.0 AND Unicode-3.0 AND Zlib
URL:            https://github.com/Petexy/LineXinBar
Source0:        linexinbar-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

# Cargo's release profile emits no DWARF, so find-debuginfo would produce an
# empty debugsourcefiles.list and rpmbuild would fail on it after the whole
# build. An archive submission wants real debuginfo instead: drop this, and
# with it the -Cdebuginfo=0 in %%build that holds Fedora's own -Cdebuginfo=2
# off, so that the DWARF is built and packaged rather than built and binned.
%global debug_package %{nil}

# Fedora appends %%{_lto_cflags} to CFLAGS, and the `cc` crate reads CFLAGS when
# it compiles the C and assembly that ring and libspa-sys build in their build
# scripts. Fedora's flags include -ffat-lto-objects, which keeps machine code
# beside the bitcode and is the only reason this links: with plain -flto those
# objects carry bitcode alone, Cargo bundles them into the rlib, and every
# symbol in them comes back undefined — which is exactly what makepkg's
# -flto=auto does on Arch. Rust's own LTO is not affected either way, because
# `[profile.release]` asks for `lto = "thin"` and Cargo is what delivers it.
%global _lto_cflags %{nil}

BuildRequires:  cargo >= 1.89
BuildRequires:  rust >= 1.89
BuildRequires:  gcc
BuildRequires:  pkgconfig
BuildRequires:  pkgconfig(alsa)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(wayland-server)
BuildRequires:  pkgconfig(libinput)
BuildRequires:  pkgconfig(libseat)
BuildRequires:  pkgconfig(libudev)
BuildRequires:  pkgconfig(libdrm)
BuildRequires:  pkgconfig(gbm)
BuildRequires:  pkgconfig(egl)
BuildRequires:  pkgconfig(glesv2)
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  pkgconfig(xkbcommon-x11)
BuildRequires:  pkgconfig(x11)
BuildRequires:  pkgconfig(xcb)
BuildRequires:  pkgconfig(xcursor)
BuildRequires:  pkgconfig(xi)
BuildRequires:  pkgconfig(libpipewire-0.3)
# A wallpaper of the user's own: their picture decoded, or their film played.
# See Settings > Appearance > Theme > Wallpaper > Custom wallpaper. RPM's ELF
# dependency generator finds these again at install time, so they are not
# repeated under Requires.
BuildRequires:  pkgconfig(libavcodec)
BuildRequires:  pkgconfig(libavformat)
BuildRequires:  pkgconfig(libavutil)
BuildRequires:  pkgconfig(libswscale)
# bindgen's, for the PipeWire bindings the portal is built on and the FFmpeg
# bindings the shell is built on.
BuildRequires:  clang

# Wayland, EGL/Vulkan and the nested X libraries are loaded dynamically, so
# RPM's ELF dependency generator cannot discover them.
#
# The compositor is a subpackage rather than part of this one so that a display
# manager can depend on a Wayland session running on this hardware without
# pulling in the shell, the portal and the Steam client behind them.
Requires:       lxb-compositor%{?_isa} = %{version}-%{release}
Requires:       fwupd
Requires:       polkit
Requires:       dbus-daemon
Requires:       dbus-tools
Requires:       libwayland-client
Requires:       libwayland-egl
Requires:       libglvnd-egl
Requires:       mesa-libEGL
Requires:       libxkbcommon
Requires:       systemd
Requires:       pipewire
Recommends:     NetworkManager
Recommends:     xdg-desktop-portal
Recommends:     wireplumber
Recommends:     pulseaudio-utils
Recommends:     hicolor-icon-theme
Recommends:     sudo
Suggests:       alsa-utils
Suggests:       ddcutil

%description
LineXinBar is a GPU-rendered, gamepad-friendly desktop shell. Every connected
display is managed as an independent output. This early-development package
also registers a native Wayland session with display managers, and runs on the
compositor in the lxb-compositor subpackage.

%package -n     lxb-compositor
Summary:        Wayland compositor for LineXinBar, usable on its own
Requires:       dbus-daemon
Requires:       libwayland-client
Requires:       libwayland-server
Requires:       libwayland-egl
Requires:       libglvnd-egl
Requires:       mesa-libEGL
Requires:       libX11
Requires:       libX11-xcb
Requires:       libxcb
Requires:       libXcursor
Requires:       libXi
Requires:       libxkbcommon-x11
Requires:       systemd
Recommends:     seatd
Recommends:     xorg-x11-server-Xwayland
Recommends:     mesa-vulkan-drivers

%package -n     lxb-retroarch
Summary:        RetroArch integration for the LineXinBar shell
# Version-locked to the shell for a sharper reason than the compositor's: what
# these two agree about is a protocol carried on a pipe, and a helper out of
# step with the shell beside it is refused outright rather than half understood.
Requires:       lxb-desktop%{?_isa} = %{version}-%{release}
Recommends:     flatpak
Suggests:       retroarch

%description -n lxb-retroarch
Adds RetroArch to the LineXinBar shell: a row under Steam in the Games column,
a column of the consoles found in a ROM folder of the user's own, and the games
in each of them.

The shell looks for this subpackage on PATH and mentions RetroArch only when it
is installed. Where RetroArch itself is missing, the shell offers to install the
Flathub build into the user's own flatpak installation, which needs no
administrative rights; a distribution package of RetroArch is preferred over it
when both are present.

%description -n lxb-compositor
A small DRM/KMS Wayland compositor that manages every connected display as an
independent output, with per-output colour management, HDR and a night light.

This subpackage is the compositor alone: it runs whatever client it is pointed
at and assumes nothing about LineXinBar's own shell. That is what lets a
display manager depend on it to put a login screen on the hardware without
installing a desktop.

%prep
%autosetup -n linexinbar-%{version}

%build
export CARGO_TARGET_DIR=target
# -Cdebuginfo=0 comes last, and it is not saying again what the release
# profile already says. RUSTFLAGS is appended after the profile's own flags
# and wins, and Fedora's %%{build_rustflags} — already exported into RUSTFLAGS
# by the time this line runs — carries -Cdebuginfo=2 -Cstrip=none. With
# %%global debug_package %%{nil} above there is no debuginfo package for that
# DWARF to be delivered in, so every crate was paying for it and then throwing
# it away: about 1.3 GiB of extra resident memory on the largest binary, which
# on a small machine is the whole difference between a build and a SIGKILL.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=%{_builddir}=/usr/src/debug/linexinbar-%{version} -Cdebuginfo=0"

# Cargo runs one rustc per binary at the end of the build and starts them
# together, and each one holds a whole crate graph at once: the release
# profile asks for thin LTO with a single codegen unit, which is what makes a
# shell of this size fast and what makes the compiler that builds it large.
# Measured peak resident size of the heaviest, lxb-desktop, on x86_64:
#
#     as this package now builds it   2072 MiB
#     with the DWARF dropped above    3348 MiB
#
# So the job count has to answer to the machine's memory and not only to its
# cores, which is all Cargo consults. Eight of these at once is what an 8 GiB
# Apple M1 died of, twice, with the kernel naming rustc and 2448 MB both times.
#
# Arithmetic here rather than %%limit_build, which is the macro for exactly
# this and cannot be used: on Fedora Asahi it swallowed the remainder of this
# script, and the build ran as the bare word `-j3`. A job count is not worth a
# macro that can do that, and this can be read by anyone holding the spec.
lxb_jobs="%{_smp_build_ncpus}"
lxb_room="$(awk '/^MemTotal:/ { n = int($2 / 1024 / 2048); print (n < 1 ? 1 : n) }' /proc/meminfo 2>/dev/null || true)"
if [ -n "$lxb_room" ] && [ "$lxb_room" -lt "$lxb_jobs" ]; then
    lxb_jobs="$lxb_room"
fi
echo "building with $lxb_jobs of %{_smp_build_ncpus} jobs, for the memory this machine has"
cargo build --frozen --release --workspace --bins -j"$lxb_jobs"

%check
export CARGO_TARGET_DIR=target
# `cargo test` already builds in the dev profile. Nothing here may pin an
# optimisation level: RUSTFLAGS is appended after the profile's own flags and
# wins, so a level named here would silently override the workspace's
# `profile.dev.package."*"` and compile every dependency unoptimised.
#
# -Cdebuginfo=0 overrides a profile setting deliberately, which is that same
# hazard turned around: the dev profile asks for full DWARF, this phase builds
# the whole graph a second time to get it, and no package is ever made of it.
# A failing test still names the file and line it failed on, because a panic
# carries its own location rather than reading DWARF.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=%{_builddir}=/usr/src/debug/linexinbar-%{version} -Cdebuginfo=0"

# The same cap as %%build, for a phase that is lighter per process and heavier
# in total: no LTO here, but a test binary for every crate in the workspace.
lxb_jobs="%{_smp_build_ncpus}"
lxb_room="$(awk '/^MemTotal:/ { n = int($2 / 1024 / 2048); print (n < 1 ? 1 : n) }' /proc/meminfo 2>/dev/null || true)"
if [ -n "$lxb_room" ] && [ "$lxb_room" -lt "$lxb_jobs" ]; then
    lxb_jobs="$lxb_room"
fi
cargo test --frozen --workspace --lib --bins -j"$lxb_jobs"
%install
export CARGO_TARGET_DIR=target
./packaging/install.sh \
    --destdir %{buildroot} \
    --prefix %{_prefix} \
    --target-dir target

# `%%license` installs a file under its basename, and the licences carried in
# this tree collide on theirs: two are called LICENSE.txt and rcheevos' is
# called LICENSE, which is the project's own name for its own. Listed as they
# are they would land on the same paths and replace one another. Copy them to
# names that say whose they are and can share a directory.
cp -p font/Roboto/LICENSE.txt Roboto-LICENSE.txt
cp -p font/RobotoMono/OFL.txt RobotoMono-OFL.txt
cp -p third_party/lxb-smithay/LICENSE.txt Smithay-LICENSE.txt
cp -p third_party/lxb-rcheevos/LICENSE rcheevos-LICENSE.txt

%files
%license LICENSE Roboto-LICENSE.txt RobotoMono-OFL.txt Smithay-LICENSE.txt rcheevos-LICENSE.txt
%doc README.md
%{_bindir}/lxb-desktop
%{_bindir}/lxb-portal
%{_bindir}/lxb-updates
%{_datadir}/polkit-1/actions/org.linexinbar.updates.policy
%{_datadir}/doc/lxb-desktop/updates.md
%{_bindir}/lxb-session
%{_datadir}/wayland-sessions/lxb.desktop
%{_datadir}/xdg-desktop-portal/portals/lxb.portal
%{_datadir}/xdg-desktop-portal/linexinbar-portals.conf
%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service
%{_datadir}/applications/linexinbar-files.desktop
%{_datadir}/applications/linexinbar-extract.desktop
%{_datadir}/applications/linexinbar-mimeapps.list
%{_prefix}/lib/udev/rules.d/70-linexinbar-input.rules

# Both packages carry the licences: they are installed and used independently,
# and a compositor on a machine with no shell still ships the terms it is
# under. The cursor theme goes here because the compositor is what loads it
# and draws the pointer from it.
%files -n       lxb-compositor
%license LICENSE Roboto-LICENSE.txt RobotoMono-OFL.txt Smithay-LICENSE.txt rcheevos-LICENSE.txt
%doc docs/configuration.md examples/config.toml
%{_bindir}/lxb
%{_datadir}/icons/Bibata-Modern-Classic/

# The integration's marks travel with its binary: they are read out of the data
# directory when the shell starts, which is how a package brings its own
# drawings to a shell that was built without them.
#
# The directory, and not a list of names. There is one mark per console and
# consoles.rs gains machines; a list here would be a second place to write that
# down, and the one nobody remembers — which is how this package came to name
# category-retroarch.svg for a release after that drawing left the tree, and to
# leave forty-five console marks installed and unpackaged. Nothing else stages
# anything under that directory, so this package owns it outright.
%files -n       lxb-retroarch
%license LICENSE Roboto-LICENSE.txt RobotoMono-OFL.txt Smithay-LICENSE.txt rcheevos-LICENSE.txt
%{_bindir}/lxb-retroarch
%{_datadir}/lxb/

%changelog
* Sun Aug 30 2026 Piotr Lewandowski <piotr.petexy@gmail.com> - 0.9.0-1
- Three hundred and forty-one commits on from the first package, and the shape
  of the shell has settled. What is new since 0.1.0, in the large:
- A file chooser of the session's own, drawn as Files in a pane over the
  application that asked for it, answering the xdg-desktop-portal FileChooser
  protocol — with the trash, making folders, marking several rows at once, and
  carrying a file somewhere else.
- Picture-in-Picture: a window that floats above everything the shell draws,
  moved and resized by hand or by a controller out of the guide, with a menu
  of its own drawn on a surface of its own.
- RetroArch as an optional package the shell finds rather than requires: your
  own games as a column, a shelf drawn at the shape of its console's boxes,
  and the BIOS asked for instead of a game that exits.
- A driver for the Steam Controller 2, which the kernel does not drive.
- The machine's accounts as a page of people, network and Bluetooth pages that
  ask the worker rather than the press, and a polkit agent in the shell so a
  password never crosses the compositor.
- A wallpaper that can be a picture or a film of your own, twelve accent
  palettes across the whole shell, and a Theme setting in two halves.
- Screen capture and sharing over the four protocols that need it, HDR asked
  and answered, frame completion reported to clients, and a display handed back
  to the application rather than only its pixels.
- OLED protection that rests a screen behind any application rather than only a
  game, and a media exception for a player that is audibly playing.
- Seventy-five marks redrawn as beads of water, one VERSION file read by both
  halves and every package, and the compositor packaged apart from the desktop.

* Sat Aug 08 2026 Piotr Lewandowski <piotr.petexiness@gmail.com> - 0.1.0-1
- Initial early-development package
