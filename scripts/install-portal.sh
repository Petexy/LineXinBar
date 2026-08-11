#!/usr/bin/env bash
#
# Register LineXinBar's desktop portal for the current user, from a build in
# this checkout.
#
# Screen sharing is not something an application does by itself: it asks
# `xdg-desktop-portal`, and that hands the question to whichever backend the
# desktop *registered*. The registration is three files, and without them a
# LineXinBar session has a portal running that nothing ever calls — the
# front desk answers every application with a portal that has no ScreenCast
# interface on it at all, which looks exactly like an application that cannot
# capture screens. OBS shows no screen-capture source; Discord's picker never
# appears.
#
# A packaged LineXinBar installs these into /usr (see packaging/install.sh).
# This is for a session run straight out of a git checkout, which installs
# nothing: the same three files, in the user's own data directory, where
# xdg-desktop-portal looks before it looks in /usr.
#
# Usage:
#   scripts/install-portal.sh              # register the build in ./target/release
#   scripts/install-portal.sh --uninstall  # take the registration away again

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
portal_file="$data_home/xdg-desktop-portal/portals/lxb.portal"
config_file="$data_home/xdg-desktop-portal/linexinbar-portals.conf"
service_file="$data_home/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service"

if [[ "${1:-}" == "--uninstall" ]]; then
    rm -f "$portal_file" "$config_file" "$service_file"
    echo "removed the portal registration from $data_home"
    exit 0
fi

if [[ $# -gt 0 ]]; then
    echo "usage: ${BASH_SOURCE[0]} [--uninstall]" >&2
    exit 2
fi

binary="$root/target/release/lxb-portal"
[[ -x "$binary" ]] || {
    echo "no portal to register at $binary — run: cargo build --release" >&2
    exit 1
}

install -Dm0644 "$root/share/xdg-desktop-portal/portals/lxb.portal" "$portal_file"
install -Dm0644 "$root/share/xdg-desktop-portal/linexinbar-portals.conf" "$config_file"

# The service file is the one that cannot be copied as it stands: it names the
# binary to start, and a checkout's is not the packaged /usr/bin path. The
# session starts its own portal, so this only matters for something that asks
# for one before the session has — but a service file pointing at a binary that
# is not there would be a portal that fails to start rather than one that is
# simply late.
install -d "$(dirname "$service_file")"
cat > "$service_file" <<EOF
[D-BUS Service]
Name=org.freedesktop.impl.portal.desktop.lxb
Exec=$binary
EOF

echo "registered $binary for this user:"
echo "  $portal_file"
echo "  $config_file"
echo "  $service_file"
echo
echo "A portal already running keeps the answers it started with, so restart it:"
echo "  systemctl --user stop xdg-desktop-portal.service"
