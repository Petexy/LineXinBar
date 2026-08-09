#!/usr/bin/env bash

set -euo pipefail

packaging_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$packaging_dir/lib.sh"

build=true
case "${1:-}" in
    --no-build) build=false ;;
    -h|--help)
        echo "Usage: packaging/check.sh [--no-build]"
        exit 0
        ;;
    "") ;;
    *) package_die "unknown check option: $1" ;;
esac

package_note "checking shell syntax"
while IFS= read -r -d '' script; do
    bash -n "$script"
done < <(find "$PACKAGING_DIR" -type f -name '*.sh' -print0)
bash -n "$PACKAGING_DIR/arch/PKGBUILD.in"

if command -v shellcheck >/dev/null 2>&1; then
    while IFS= read -r -d '' script; do
        shellcheck -x "$script"
    done < <(find "$PACKAGING_DIR" -type f -name '*.sh' -print0)
fi

package_note "checking the package version is consistent"
# The package and the application report the same version, so the workspace is
# the other half of this check: `lxb --version` disagreeing with the package it
# was installed from is the kind of thing nobody notices until a bug report.
workspace_version="$(awk '
    /^\[/ { section = $0; next }
    section == "[workspace.package]" \
        && match($0, /^version[[:space:]]*=[[:space:]]*"[^"]+"/) {
        line = substr($0, RSTART, RLENGTH)
        sub(/^version[[:space:]]*=[[:space:]]*"/, "", line)
        sub(/"$/, "", line)
        print line
        exit
    }' "$PROJECT_ROOT/Cargo.toml")"
[[ -n "$workspace_version" ]] \
    || package_die "could not read [workspace.package] version from Cargo.toml"
[[ "$workspace_version" == "$PACKAGE_VERSION" ]] \
    || package_die "packaging/VERSION says $PACKAGE_VERSION but the workspace says $workspace_version"
# The spec carries a literal version, so compare it against VERSION itself: a
# pattern spelling out the version would agree with a stale spec forever, and
# the mismatch would only surface as rpmbuild failing to find its Source0.
grep -Eq "^Version:[[:space:]]+${PACKAGE_VERSION//./\\.}\$" \
    "$PACKAGING_DIR/fedora/linexinbar.spec" \
    || package_die "fedora/linexinbar.spec does not declare version $PACKAGE_VERSION"
# The rest take the version from VERSION, so check that they still do.
grep -Fqx 'pkgver=@VERSION@' "$PACKAGING_DIR/arch/PKGBUILD.in" \
    || package_die "arch/PKGBUILD.in no longer reads its version from packaging/VERSION"
grep -Fq 'builtins.readFile ../VERSION' "$PACKAGING_DIR/nix/package.nix" \
    || package_die "nix/package.nix no longer reads its version from packaging/VERSION"
grep -Fq 'Version: @VERSION@-1' "$PACKAGING_DIR/debian/control.in" \
    || package_die "debian/control.in no longer reads its version from packaging/VERSION"

if command -v rpmspec >/dev/null 2>&1; then
    rpmspec --parse "$PACKAGING_DIR/fedora/linexinbar.spec" >/dev/null
fi
if command -v nix-instantiate >/dev/null 2>&1; then
    nix-instantiate --parse "$PROJECT_ROOT/flake.nix" >/dev/null
    nix-instantiate --parse "$PACKAGING_DIR/nix/package.nix" >/dev/null
    nix-instantiate --parse "$PACKAGING_DIR/nix/module.nix" >/dev/null
fi

require_command cargo
require_rust_version 1.89
if [[ "$build" == true ]]; then
    package_note "building the two release binaries"
    cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" \
        --release --locked --workspace --bins
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/linexinbar-check.XXXXXX")"
cleanup() {
    if [[ -n "${work:-}" && "$work" == */linexinbar-check.* && -d "$work" ]]; then
        rm -rf -- "$work"
    fi
}
trap cleanup EXIT

stage="$work/stage"
# No --target-dir: install.sh already follows CARGO_TARGET_DIR, which is where
# the build above put the binaries.
"$PACKAGING_DIR/install.sh" --destdir "$stage"

package_note "checking the staged desktop and session payload"
for binary in lxb lxb-desktop lxb-session; do
    [[ -x "$stage/usr/bin/$binary" ]] || package_die "staged binary is missing: $binary"
done
session="$stage/usr/share/wayland-sessions/lxb.desktop"
[[ -f "$session" ]] || package_die "Wayland session entry was not staged"
for line in Exec=lxb-session TryExec=lxb-session DesktopNames=LineXinBar; do
    grep -qx "$line" "$session" \
        || package_die "the Wayland session entry is missing: $line"
done

# DesktopNames is a display-manager session key used in real session files,
# but the generic desktop-entry validator does not know it. Validate every
# standard field on a temporary copy and assert DesktopNames separately above.
if command -v desktop-file-validate >/dev/null 2>&1; then
    sed '/^DesktopNames=/d' "$session" > "$work/lxb-standard.desktop"
    desktop-file-validate "$work/lxb-standard.desktop"
fi

source_theme="$PROJECT_ROOT/share/icons/Bibata-Modern-Classic"
staged_theme="$stage/usr/share/icons/Bibata-Modern-Classic"
source_links="$(find "$source_theme" -type l | wc -l | tr -d ' ')"
staged_links="$(find "$staged_theme" -type l | wc -l | tr -d ' ')"
[[ "$source_links" -gt 0 && "$source_links" == "$staged_links" ]] \
    || package_die "cursor symlinks were not preserved ($source_links source, $staged_links staged)"
if [[ -n "$(find -L "$staged_theme" -type l -print -quit)" ]]; then
    package_die "staged cursor theme contains a broken symlink"
fi

package_note "package definitions and staged payload are valid (version $PACKAGE_VERSION)"
