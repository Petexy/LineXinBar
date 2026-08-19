Name:           lxb-desktop
Version:        0.1.0
Release:        1%{?dist}
Summary:        Multi-display Wayland desktop with an XMB-style shell

# LineXinBar/Bibata, embedded Roboto, and the locked statically linked Rust
# dependency graph for Linux.
License:        GPL-3.0-only AND Apache-2.0 AND MIT AND BSD-2-Clause AND BSD-3-Clause AND ISC AND MPL-2.0 AND Unicode-3.0 AND Zlib
URL:            https://github.com/Petexy/LineXinBar
Source0:        linexinbar-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

# Cargo's release profile emits no DWARF, so find-debuginfo would produce an
# empty debugsourcefiles.list and rpmbuild would fail on it after the whole
# build. An archive submission wants real debuginfo instead: drop this and
# build with `-Cdebuginfo=2 -Cstrip=none` under Fedora's own remapping.
%global debug_package %{nil}

# Fedora appends %{_lto_cflags} to CFLAGS, and the `cc` crate reads CFLAGS when
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
BuildRequires:  clang

# Wayland, EGL/Vulkan and the nested X libraries are loaded dynamically, so
# RPM's ELF dependency generator cannot discover them.
#
# The compositor is a subpackage rather than part of this one so that a display
# manager can depend on a Wayland session running on this hardware without
# pulling in the shell, the portal and the Steam client behind them.
Requires:       lxb-compositor%{?_isa} = %{version}-%{release}
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
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=%{_builddir}=/usr/src/debug/linexinbar-%{version}"
cargo build --frozen --release --workspace --bins

%check
export CARGO_TARGET_DIR=target
# `cargo test` already builds in the dev profile. Nothing here may pin an
# optimisation level: RUSTFLAGS is appended after the profile's own flags and
# wins, so a level named here would silently override the workspace's
# `profile.dev.package."*"` and compile every dependency unoptimised.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=%{_builddir}=/usr/src/debug/linexinbar-%{version}"
cargo test --frozen --workspace --lib --bins

%install
export CARGO_TARGET_DIR=target
./packaging/install.sh \
    --destdir %{buildroot} \
    --prefix %{_prefix} \
    --target-dir target

# `%%license` installs a file under its basename, and the two third-party
# licences carried in this tree are both called LICENSE.txt: listed as they are
# they would land on the same path and one would replace the other. Copy them
# to names that say whose they are and can share a directory.
cp -p font/Roboto/LICENSE.txt Roboto-LICENSE.txt
cp -p third_party/smithay/LICENSE.txt Smithay-LICENSE.txt

%files
%license LICENSE Roboto-LICENSE.txt Smithay-LICENSE.txt
%doc README.md
%{_bindir}/lxb-desktop
%{_bindir}/lxb-portal
%{_bindir}/lxb-session
%{_datadir}/wayland-sessions/lxb.desktop
%{_datadir}/xdg-desktop-portal/portals/lxb.portal
%{_datadir}/xdg-desktop-portal/linexinbar-portals.conf
%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service

# Both packages carry the licences: they are installed and used independently,
# and a compositor on a machine with no shell still ships the terms it is
# under. The cursor theme goes here because the compositor is what loads it
# and draws the pointer from it.
%files -n       lxb-compositor
%license LICENSE Roboto-LICENSE.txt Smithay-LICENSE.txt
%doc docs/configuration.md examples/config.toml
%{_bindir}/lxb
%{_datadir}/icons/Bibata-Modern-Classic/

%changelog
* Sat Aug 08 2026 Piotr Lewandowski <piotr.petexiness@gmail.com> - 0.1.0-1
- Initial early-development package
