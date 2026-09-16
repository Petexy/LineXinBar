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
                            [--component compositor|desktop|retroarch|all]

Stages one part of LineXinBar, or all of them. PREFIX defaults to /usr and
--component to all.

  compositor  lxb and the bundled cursor theme it draws the pointer from.
              Everything needed to run a Wayland session and nothing that
              assumes this project's shell is the one being run — which is
              what lets a display manager depend on it alone.
  desktop     lxb-desktop, lxb-portal, lxb-updates, the packaged session launcher, the
              Wayland session entry and the desktop portal's registration.
              Useless without the compositor; the packages say so.
  retroarch   lxb-retroarch and the two marks it draws its rows with: the
              optional RetroArch integration. Nothing else needs it, the shell
              looks for it on PATH, and a machine without it has a shell that
              never mentions RetroArch at all.
  all         Every part, as one tree.
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
    compositor|desktop|retroarch|all) ;;
    *) package_die "unknown component: $component (compositor, desktop, retroarch or all)" ;;
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
    install_binary lxb-updates
    local policy="$install_root/share/polkit-1/actions/org.linexinbar.updates.policy"
    install -Dm0644 "$PACKAGING_DIR/files/org.linexinbar.updates.policy.in" "$policy"
    sed -i "s|@HELPER@|$prefix/bin/lxb-updates|g" "$policy"
    install -Dm0644 "$PROJECT_ROOT/docs/updates.md" "$install_root/share/doc/lxb-desktop/updates.md"

    install -Dm0755 "$PACKAGING_DIR/files/lxb-session" "$install_root/bin/lxb-session"
    install -Dm0644 "$PACKAGING_DIR/files/lxb.desktop" \
        "$install_root/share/wayland-sessions/lxb.desktop"

    # The two device nodes the shell opens itself, neither of which is granted
    # to anybody by default. Without them the controller handling half works
    # and says nothing about why — see the file's own notes, and
    # `crates/lxb-desktop/src/pad_guard.rs`. In the desktop package because the
    # shell is what opens them: a compositor on its own has no use for either.
    install -Dm0644 "$PACKAGING_DIR/files/70-linexinbar-input.rules" \
        "$install_root/lib/udev/rules.d/70-linexinbar-input.rules"

    # Being the machine's file manager, which is two files and no daemon. The
    # shell takes `org.freedesktop.FileManager1` on the session bus while it
    # runs, which is what a browser's Show in folder calls; these cover the
    # other road to the same place — `xdg-open` on a folder, and every
    # application that falls back to launching whatever opens one.
    #
    # The list is deliberately desktop-specific. It is read only while
    # `XDG_CURRENT_DESKTOP` lowercases to `linexinbar`, so installing this
    # package does not take folders away from the file manager of whatever
    # else is on the machine, and nothing is ever written into the user's own
    # `mimeapps.list`. See share/applications/linexinbar-mimeapps.list.
    install -Dm0644 "$PROJECT_ROOT/share/applications/linexinbar-files.desktop" \
        "$install_root/share/applications/linexinbar-files.desktop"
    install -Dm0644 "$PROJECT_ROOT/share/applications/linexinbar-mimeapps.list" \
        "$install_root/share/applications/linexinbar-mimeapps.list"

    # And the entry standing for the shell's own Extract, which is what opens an
    # archive in this session. It is not a program: the shell answers a press on
    # a `.zip` itself, and this exists so the machine has something to *name* as
    # the handler — a choice the user makes on the Open with list is written
    # into their own `mimeapps.list` as a desktop entry name, so an answer with
    # no entry to its name could be displaced and never chosen back. It is also
    # the road in from outside, `xdg-open` on an archive, which runs the shell
    # with `--extract`. See crates/lxb-desktop/src/archive.rs.
    #
    # Deliberately not named in the list above. Extract is the default *in the
    # shell*, which the shell decides for itself and needs no file for; naming
    # it there as well would take archives away from whatever else on the
    # machine opens them, in a session where this desktop is only the one the
    # user happens to be in.
    install -Dm0644 "$PROJECT_ROOT/share/applications/linexinbar-extract.desktop" \
        "$install_root/share/applications/linexinbar-extract.desktop"

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

# The optional half: an integration for one program, in a package of its own so
# that a machine which will never emulate a console does not carry it. The shell
# finds `lxb-retroarch` on PATH and grows a RetroArch row when it is there; its
# marks are read out of share/lxb/glyphs at startup, which is how a package
# brings its own drawings to a shell that was built without them. See
# `crates/lxb-retroarch` and `lxb-desktop`'s `src/retroarch.rs`.
stage_retroarch() {
    install_binary lxb-retroarch

    local glyphs="$PROJECT_ROOT/crates/lxb-retroarch/glyphs"
    local drawing
    local staged=0
    for drawing in "$glyphs"/*.svg; do
        [[ -f "$drawing" ]] || continue
        install -Dm0644 "$drawing" "$install_root/share/lxb/glyphs/$(basename "$drawing")"
        staged=$((staged + 1))
    done
    # Without them every row of that column falls back to the shell's own pad,
    # which is a package that half works and says nothing about it.
    ((staged > 0)) || package_die "no glyphs to stage from $glyphs"
}

if [[ "$component" == compositor || "$component" == all ]]; then
    stage_compositor
fi
if [[ "$component" == desktop || "$component" == all ]]; then
    stage_desktop
fi
if [[ "$component" == retroarch || "$component" == all ]]; then
    stage_retroarch
fi
