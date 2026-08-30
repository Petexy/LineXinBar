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
for control in control.in control-compositor.in control-retroarch.in; do
    grep -Fq 'Version: @VERSION@-1' "$PACKAGING_DIR/debian/$control" \
        || package_die "debian/$control no longer reads its version from VERSION"
done
# The two binaries read the same file, and each says so for itself: a build
# script that has stopped looking is a `--version` free to drift from the
# package it ships in, which is what this whole section exists to prevent.
for component in lxb-compositor lxb-desktop lxb-retroarch; do
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
grep -Fq 'lxb-desktop (= @VERSION@-1)' "$PACKAGING_DIR/debian/control-retroarch.in" \
    || package_die "debian/control-retroarch.in no longer depends on the matching lxb-desktop"
grep -Fq '"lxb-desktop=$pkgver-$pkgrel"' "$PACKAGING_DIR/arch/PKGBUILD.in" \
    || package_die "arch/PKGBUILD.in no longer version-locks lxb-retroarch to the shell"
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
    package_note "building the release binaries"
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
"$PACKAGING_DIR/install.sh" --destdir "$work/retroarch" --component retroarch

package_note "checking the staged desktop and session payload"
for binary in lxb lxb-desktop lxb-session; do
    [[ -x "$stage/usr/bin/$binary" ]] || package_die "staged binary is missing: $binary"
done

package_note "checking the three components partition the payload"
# The compositor is a package of its own so a display manager can depend on a
# Wayland session without pulling in this project's shell, and the RetroArch
# integration is one so that a machine which will never emulate a console does
# not carry it. That only holds while the split stays a partition: anything
# installed by no component is a file that quietly stops shipping, and anything
# installed by two is a file two packages will fight over at install time.
staged_paths() {
    (cd "$1" && find . -mindepth 1 \( -type f -o -type l \) -printf '%P\n' | sort)
}
staged_paths "$stage" > "$work/all.list"
for component in compositor desktop retroarch; do
    staged_paths "$work/$component" > "$work/$component.list"
done

for pair in "compositor desktop" "compositor retroarch" "desktop retroarch"; do
    read -r one other <<< "$pair"
    comm -12 "$work/$one.list" "$work/$other.list" > "$work/both.list"
    if [[ -s "$work/both.list" ]]; then
        package_die "$one and $other both install: $(tr '\n' ' ' < "$work/both.list")"
    fi
done
sort -u "$work/compositor.list" "$work/desktop.list" "$work/retroarch.list" \
    > "$work/union.list"
if ! diff -q "$work/all.list" "$work/union.list" >/dev/null; then
    package_die "--component all differs from the three components together: $(
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
# And the optional one is optional in the direction that matters: the shell must
# not be carrying the integration's binary, or installing the shell would
# install the integration and the whole point of the split would be gone.
[[ ! -e "$work/desktop/usr/bin/lxb-retroarch" ]] \
    || package_die "the desktop component stages lxb-retroarch, which is a package of its own"
[[ -x "$work/retroarch/usr/bin/lxb-retroarch" ]] \
    || package_die "the retroarch component does not stage lxb-retroarch"
# Its marks travel with it and are read out of the data directory at startup:
# a package with the binary and no drawings is a column of rows wearing the
# shell's fallback pad. See `icons::package_glyphs`.
#
# The one named mark is the fallback every RetroArch row wears; the rest are a
# console each and are checked as a set rather than by name, because the set
# grows whenever `consoles.rs` learns another machine and a list here would be
# a second place for that to be written down.
marks="$work/retroarch/usr/share/lxb/glyphs"
[[ -f "$marks/retroarch.svg" ]] \
    || package_die "the retroarch component does not stage its own mark"
consoles=$(find "$marks" -name 'console-*.svg' | wc -l)
(( consoles >= 40 )) \
    || package_die "the retroarch component stages only $consoles console marks"
for mark in "$marks"/*.svg; do
    grep -Fq 'lxb:shape' "$mark" \
        || package_die "$(basename "$mark") no longer says it ships as the shape of itself"
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

# Being the machine's file manager, which is three facts that have to agree.
# The half that matters most needs nothing staged — the running shell takes
# `org.freedesktop.FileManager1` itself — but the other road, `xdg-open` on a
# folder, is an entry claiming the type, a list naming that entry, and the
# desktop name the list is keyed to. Any one of them alone does nothing, and
# nothing says so at runtime: a folder simply opens somebody else's file
# manager.
handler="$stage/usr/share/applications/linexinbar-files.desktop"
defaults="$stage/usr/share/applications/linexinbar-mimeapps.list"
[[ -f "$handler" ]] || package_die "the folder handler was not staged"
[[ -f "$defaults" ]] || package_die "the folder handler is named as the default by nothing"
grep -qx 'MimeType=inode/directory;' "$handler" \
    || package_die "the folder handler does not claim inode/directory"
grep -qx 'inode/directory=linexinbar-files.desktop' "$defaults" \
    || package_die "the defaults do not name the folder handler"
# The list is read only while XDG_CURRENT_DESKTOP lowercases to the name in
# front of `-mimeapps.list`, which is the whole of what keeps it off every
# other desktop on this machine — and the session is what sets it.
grep -qx 'export XDG_CURRENT_DESKTOP=LineXinBar' "$PACKAGING_DIR/files/lxb-session" \
    || package_die "the session no longer sets the desktop name the defaults are keyed to"
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$handler"
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
