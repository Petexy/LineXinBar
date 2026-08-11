Name:           linexinbar
Version:        0.1.0
Release:        1%{?dist}
Summary:        Multi-display Wayland desktop with an XMB-style shell

# LineXinBar/Bibata, embedded Roboto, and the locked statically linked Rust
# dependency graph for Linux.
License:        GPL-3.0-only AND Apache-2.0 AND MIT AND BSD-2-Clause AND BSD-3-Clause AND ISC AND MPL-2.0 AND Unicode-3.0 AND Zlib
URL:            https://github.com/petexy/project-linexinbar
Source0:        %{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

# Cargo's release profile emits no DWARF, so find-debuginfo would produce an
# empty debugsourcefiles.list and rpmbuild would fail on it after the whole
# build. An archive submission wants real debuginfo instead: drop this and
# build with `-Cdebuginfo=2 -Cstrip=none` under Fedora's own remapping.
%global debug_package %{nil}

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
Requires:       dbus-daemon
Requires:       dbus-tools
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
Requires:       pipewire
Recommends:     xdg-desktop-portal
Recommends:     seatd
Recommends:     xorg-x11-server-Xwayland
Recommends:     mesa-vulkan-drivers
Recommends:     wireplumber
Recommends:     pulseaudio-utils
Recommends:     hicolor-icon-theme
Recommends:     sudo
Suggests:       alsa-utils
Suggests:       ddcutil

%description
LineXinBar combines a small DRM/KMS Wayland compositor with a GPU-rendered,
gamepad-friendly desktop shell. Every connected display is managed as an
independent output. This early-development package also registers a native
Wayland session with display managers.

%prep
%autosetup -n %{name}-%{version}

%build
export CARGO_TARGET_DIR=target
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=%{_builddir}=/usr/src/debug/%{name}-%{version}"
cargo build --frozen --release --workspace --bins

%check
export CARGO_TARGET_DIR=target
# `cargo test` already builds in the dev profile. Nothing here may pin an
# optimisation level: RUSTFLAGS is appended after the profile's own flags and
# wins, so a level named here would silently override the workspace's
# `profile.dev.package."*"` and compile every dependency unoptimised.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=%{_builddir}=/usr/src/debug/%{name}-%{version}"
cargo test --frozen --workspace --lib --bins

%install
export CARGO_TARGET_DIR=target
./packaging/install.sh \
    --destdir %{buildroot} \
    --prefix %{_prefix} \
    --target-dir target

%files
%license LICENSE font/Roboto/LICENSE.txt
%doc README.md docs/configuration.md examples/config.toml
%{_bindir}/lxb
%{_bindir}/lxb-desktop
%{_bindir}/lxb-portal
%{_bindir}/lxb-session
%{_datadir}/wayland-sessions/lxb.desktop
%{_datadir}/xdg-desktop-portal/portals/lxb.portal
%{_datadir}/xdg-desktop-portal/linexinbar-portals.conf
%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service
%{_datadir}/icons/Bibata-Modern-Classic/

%changelog
* Sat Aug 08 2026 Piotr Lewandowski <piotr.petexiness@gmail.com> - 0.1.0-1
- Initial early-development package
