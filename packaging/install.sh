#!/usr/bin/env bash

# Stage the runtime payload shared by every distro package. This deliberately
# does not install distro-specific documentation or license metadata.

set -euo pipefail

packaging_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$packaging_dir/lib.sh"

destdir=""
prefix="/usr"
component="all"
target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"

usage() {
    cat <<'EOF'
Usage: packaging/install.sh --destdir DIR [--prefix PREFIX] [--target-dir DIR]
                            [--component compositor|desktop|all]

Stages one half of LineXinBar, or both. PREFIX defaults to /usr and
--component to all.

  compositor  lxb and the bundled cursor theme it draws the pointer from.
              Everything needed to run a Wayland session and nothing that
              assumes this project's shell is the one being run — which is
              what lets a display manager depend on it alone.
  desktop     lxb-desktop, lxb-portal, the packaged session launcher, the
              Wayland session entry and the desktop portal's registration.
              Useless without the compositor; the packages say so.
  all         Both, as one tree.
EOF
}

while (($#)); do
    case "$1" in
        --destdir)
            (($# >= 2)) || package_die "--destdir requires a value"
            destdir="$2"
            shift 2
            ;;
        --prefix)
            (($# >= 2)) || package_die "--prefix requires a value"
            prefix="$2"
            shift 2
            ;;
        --target-dir)
            (($# >= 2)) || package_die "--target-dir requires a value"
            target_dir="$2"
            shift 2
            ;;
        --component)
            (($# >= 2)) || package_die "--component requires a value"
            component="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *) package_die "unknown install option: $1" ;;
    esac
done

case "$component" in
    compositor|desktop|all) ;;
    *) package_die "unknown component: $component (compositor, desktop or all)" ;;
esac

[[ -n "$destdir" ]] || package_die "--destdir is required"
[[ "$destdir" == /* ]] || package_die "--destdir must be absolute"
[[ -z "$prefix" || "$prefix" == /* ]] || package_die "--prefix must be empty or absolute"

if [[ "$target_dir" != /* ]]; then
    target_dir="$PROJECT_ROOT/$target_dir"
fi
prefix="${prefix%/}"
[[ "$prefix" != "/" ]] || prefix=""
install_root="${destdir}${prefix}"

install_binary() {
    local binary="$1"
    [[ -x "$target_dir/release/$binary" ]] \
        || package_die "missing release binary: $target_dir/release/$binary"
    install -Dm0755 "$target_dir/release/$binary" "$install_root/bin/$binary"
}

# The compositor half: a Wayland session on DRM/KMS, and nothing that assumes
# this project's shell is the one running in it. A display manager needs
# exactly this much of LineXinBar to put a login screen on the hardware, which
# is why it is a package of its own.
stage_compositor() {
    install_binary lxb

    # The pointer belongs to the compositor — it is what loads the theme and
    # draws the cursor — so the theme ships beside it rather than with the
    # shell. See `general.cursor_theme` in the configuration.
    local cursor_source="$PROJECT_ROOT/share/icons/Bibata-Modern-Classic"
    local cursor_destination="$install_root/share/icons/Bibata-Modern-Classic"
    [[ -d "$cursor_source/cursors" ]] || package_die "bundled cursor theme is missing"
    mkdir -p "$cursor_destination"
    # Preserve cursor aliases and metadata, but let the package builder assign
    # ownership. Fakeroot cannot reliably emulate ownership changes on symlinks.
    cp -a --no-preserve=ownership "$cursor_source/." "$cursor_destination/"
}

# The desktop half: the shell, the portal that lets the rest of the system ask
# it for a piece of the screen, and the session entry a display manager offers.
# None of it runs without the compositor above.
stage_desktop() {
    install_binary lxb-desktop
    install_binary lxb-portal

    install -Dm0755 "$PACKAGING_DIR/files/lxb-session" "$install_root/bin/lxb-session"
    install -Dm0644 "$PACKAGING_DIR/files/lxb.desktop" \
        "$install_root/share/wayland-sessions/lxb.desktop"

    # The desktop portal: how an application outside the session asks for a
    # piece of it. The `.portal` file is what xdg-desktop-portal reads to find
    # this backend at all, the `.conf` says which questions it answers for a
    # LineXinBar session, and the D-Bus service file lets it be started on
    # demand by anything that asks before the session has.
    install -Dm0644 "$PROJECT_ROOT/share/xdg-desktop-portal/portals/lxb.portal" \
        "$install_root/share/xdg-desktop-portal/portals/lxb.portal"
    install -Dm0644 "$PROJECT_ROOT/share/xdg-desktop-portal/linexinbar-portals.conf" \
        "$install_root/share/xdg-desktop-portal/linexinbar-portals.conf"
    install -Dm0644 \
        "$PROJECT_ROOT/share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service" \
        "$install_root/share/dbus-1/services/org.freedesktop.impl.portal.desktop.lxb.service"
}

if [[ "$component" == compositor || "$component" == all ]]; then
    stage_compositor
fi
if [[ "$component" == desktop || "$component" == all ]]; then
    stage_desktop
fi
