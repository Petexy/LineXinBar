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

# The version bumper lives in scripts/ rather than in here, because the version
# is not a packaging detail — but it is the thing that writes the manifest and
# the spec this file checks, so it is checked with them.
shell_scripts() {
    find "$PACKAGING_DIR" -type f -name '*.sh' -print0
    printf '%s\0' "$PROJECT_ROOT/scripts/bump-version.sh"
}

package_note "checking shell syntax"
while IFS= read -r -d '' script; do
    bash -n "$script"
done < <(shell_scripts)
bash -n "$PACKAGING_DIR/arch/PKGBUILD.in"

if command -v shellcheck >/dev/null 2>&1; then
    while IFS= read -r -d '' script; do
        shellcheck -x "$script"
    done < <(shell_scripts)
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
    || package_die "VERSION says $PACKAGE_VERSION but the workspace says $workspace_version.
Run: scripts/bump-version.sh $PACKAGE_VERSION"
# The spec carries a literal version — `Version:` has to be one for the spec to
# be a spec anyone could submit — so compare it against VERSION itself: a
# pattern spelling out the version would agree with a stale spec forever, and
# the mismatch would only surface as rpmbuild failing to find its Source0.
# scripts/bump-version.sh is what writes this one and the manifest above.
grep -Eq "^Version:[[:space:]]+${PACKAGE_VERSION//./\\.}\$" \
    "$PACKAGING_DIR/fedora/lxb-desktop.spec" \
    || package_die "fedora/lxb-desktop.spec does not declare version $PACKAGE_VERSION"
# The rest take the version from VERSION, so check that they still do.
grep -Fqx 'pkgver=@VERSION@' "$PACKAGING_DIR/arch/PKGBUILD.in" \
    || package_die "arch/PKGBUILD.in no longer reads its version from VERSION"
grep -Fq 'builtins.readFile ../../VERSION' "$PACKAGING_DIR/nix/package.nix" \
    || package_die "nix/package.nix no longer reads its version from VERSION"
for control in control.in control-compositor.in; do
    grep -Fq 'Version: @VERSION@-1' "$PACKAGING_DIR/debian/$control" \
        || package_die "debian/$control no longer reads its version from VERSION"
done
# The two binaries read the same file, and each says so for itself: a build
# script that has stopped looking is a `--version` free to drift from the
# package it ships in, which is what this whole section exists to prevent.
for component in lxb-compositor lxb-desktop; do
    build_script="$PROJECT_ROOT/crates/$component/build.rs"
    [[ -f "$build_script" ]] \
        || package_die "crates/$component/build.rs is gone, and with it the check that it builds as the version in VERSION"
    grep -Fq 'join("VERSION")' "$build_script" \
        || package_die "crates/$component/build.rs no longer reads its version from VERSION"
done
# The desktop is useless without the compositor, and says so with a version
# lock rather than a bare name: two halves of one build that drift apart are a
# shell talking to a compositor it was never tested against.
grep -Fq 'lxb-compositor (= @VERSION@-1)' "$PACKAGING_DIR/debian/control.in" \
    || package_die "debian/control.in no longer depends on the matching lxb-compositor"
grep -Fq '"lxb-compositor=$pkgver-$pkgrel"' "$PACKAGING_DIR/arch/PKGBUILD.in" \
    || package_die "arch/PKGBUILD.in no longer depends on the matching lxb-compositor"
grep -Fq 'Requires:       lxb-compositor%{?_isa} = %{version}-%{release}' \
    "$PACKAGING_DIR/fedora/lxb-desktop.spec" \
    || package_die "fedora/lxb-desktop.spec no longer depends on the matching compositor subpackage"

if command -v rpmspec >/dev/null 2>&1; then
    rpmspec --parse "$PACKAGING_DIR/fedora/lxb-desktop.spec" >/dev/null
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

work="$(package_work_dir linexinbar-check)"
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
"$PACKAGING_DIR/install.sh" --destdir "$work/compositor" --component compositor
"$PACKAGING_DIR/install.sh" --destdir "$work/desktop" --component desktop

package_note "checking the staged desktop and session payload"
for binary in lxb lxb-desktop lxb-session; do
    [[ -x "$stage/usr/bin/$binary" ]] || package_die "staged binary is missing: $binary"
done

package_note "checking the two components partition the payload"
# The compositor is a package of its own so a display manager can depend on a
# Wayland session without pulling in this project's shell. That only holds
# while the split stays a partition: anything installed by neither component
# is a file that quietly stops shipping, and anything installed by both is a
# file two packages will fight over at install time.
staged_paths() {
    (cd "$1" && find . -mindepth 1 \( -type f -o -type l \) -printf '%P\n' | sort)
}
staged_paths "$stage" > "$work/all.list"
staged_paths "$work/compositor" > "$work/compositor.list"
staged_paths "$work/desktop" > "$work/desktop.list"

comm -12 "$work/compositor.list" "$work/desktop.list" > "$work/both.list"
if [[ -s "$work/both.list" ]]; then
    package_die "both components install: $(tr '\n' ' ' < "$work/both.list")"
fi
sort -u "$work/compositor.list" "$work/desktop.list" > "$work/union.list"
if ! diff -q "$work/all.list" "$work/union.list" >/dev/null; then
    package_die "--component all differs from compositor plus desktop: $(
        diff "$work/all.list" "$work/union.list" | tr '\n' ' ')"
fi

# And the halves have to be the right halves. A shell binary in the compositor
# package would defeat the point of splitting them.
[[ -x "$work/compositor/usr/bin/lxb" ]] \
    || package_die "the compositor component does not stage lxb"
[[ -d "$work/compositor/usr/share/icons/Bibata-Modern-Classic/cursors" ]] \
    || package_die "the compositor component does not stage the cursor theme it draws with"
for intruder in lxb-desktop lxb-portal lxb-session; do
    [[ ! -e "$work/compositor/usr/bin/$intruder" ]] \
        || package_die "the compositor component stages $intruder, which belongs to the desktop"
done
[[ ! -e "$work/desktop/usr/bin/lxb" ]] \
    || package_die "the desktop component stages the compositor it is supposed to depend on"
for expected in lxb-desktop lxb-portal lxb-session; do
    [[ -x "$work/desktop/usr/bin/$expected" ]] \
        || package_die "the desktop component does not stage $expected"
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
