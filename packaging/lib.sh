#!/usr/bin/env bash

# Shared helpers for the distro package builders. This file is sourced; it is
# not an entry point on its own.

PACKAGING_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"
# `read` reports failure on a final line with no newline, which under `set -e`
# would end the caller with no explanation. Take the value either way and let
# the pattern below be the one thing that rejects it.
IFS= read -r PACKAGE_VERSION < "$PACKAGING_DIR/VERSION" || true

if [[ ! "$PACKAGE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "invalid package version in $PACKAGING_DIR/VERSION: $PACKAGE_VERSION" >&2
    exit 1
fi

package_die() {
    echo "error: $*" >&2
    exit 1
}

package_note() {
    echo "==> $*"
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || package_die "required command not found: $1"
}

host_is_like() {
    local wanted="$1"

    # In a subshell: /etc/os-release assigns a dozen names (NAME, VERSION,
    # BUILD_ID …) and none of them belong in a build script's environment.
    (
        if [[ -r /etc/os-release ]]; then
            # Distribution-supplied shell assignments only.
            # shellcheck disable=SC1091
            source /etc/os-release
        fi
        [[ " ${ID:-} ${ID_LIKE:-} " == *" $wanted "* ]]
    )
}

require_rust_version() {
    local minimum="${1:-1.89}"
    local actual
    local first

    require_command rustc
    require_command cargo
    actual="$(rustc --version | awk '{print $2}')"
    first="$(printf '%s\n%s\n' "$minimum" "$actual" | sort -V | head -n 1)"
    if [[ "$first" != "$minimum" ]]; then
        package_die "Rust $minimum or newer is required by the locked dependency graph (found $actual)"
    fi
}

package_source_date_epoch() {
    if [[ -n "${SOURCE_DATE_EPOCH:-}" ]]; then
        [[ "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] \
            || package_die "SOURCE_DATE_EPOCH must be an integer"
        printf '%s\n' "$SOURCE_DATE_EPOCH"
        return
    fi

    git -C "$PROJECT_ROOT" log -1 --format=%ct HEAD 2>/dev/null || date +%s
}

snapshot_source() {
    local destination="$1"

    [[ "$destination" == /* ]] || package_die "snapshot destination must be absolute"
    mkdir -p "$destination"
    if [[ -n "$(find "$destination" -mindepth 1 -print -quit)" ]]; then
        package_die "snapshot destination is not empty: $destination"
    fi

    # Use tracked files plus non-ignored working-tree additions, rather than
    # git archive, so an intentional local build contains the exact sources
    # the developer is testing. Asking Git for the list also keeps ignored
    # build output, editor state and local .env files out of source packages.
    while IFS= read -r -d '' path; do
        case "$path" in
            packaging/out|packaging/out/*|result|result-*) continue ;;
        esac
        if [[ -e "$PROJECT_ROOT/$path" || -L "$PROJECT_ROOT/$path" ]]; then
            printf '%s\0' "$path"
        fi
    done < <(git -C "$PROJECT_ROOT" ls-files -z --cached --others --exclude-standard) \
        | tar --null --no-recursion -C "$PROJECT_ROOT" -T - -cf - \
        | tar -C "$destination" -xf -
}

archive_snapshot() {
    local source_dir="$1"
    local output="$2"
    local epoch
    local source_parent
    local source_name

    [[ -d "$source_dir" ]] || package_die "snapshot directory does not exist: $source_dir"
    [[ "$output" == /* ]] || package_die "archive output path must be absolute"
    [[ ! -e "$output" ]] || package_die "refusing to overwrite archive: $output"

    epoch="$(package_source_date_epoch)"
    source_parent="$(dirname "$source_dir")"
    source_name="$(basename "$source_dir")"
    mkdir -p "$(dirname "$output")"

    tar --sort=name \
        --mtime="@$epoch" \
        --owner=0 --group=0 --numeric-owner \
        -C "$source_parent" -cf - "$source_name" | gzip -n > "$output"
}
