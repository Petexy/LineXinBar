#!/usr/bin/env bash

# Shared helpers for the distro package builders. This file is sourced; it is
# not an entry point on its own.

PACKAGING_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"
# The one number this project releases under. It lives at the root of the
# checkout rather than in here because it is not a packaging detail: the
# compositor and the shell both read the same file while they build, and refuse
# to build against a manifest that has drifted from it. See scripts/bump-version.sh.
#
# `read` reports failure on a final line with no newline, which under `set -e`
# would end the caller with no explanation. Take the value either way and let
# the pattern below be the one thing that rejects it.
PACKAGE_VERSION_FILE="$PROJECT_ROOT/VERSION"
IFS= read -r PACKAGE_VERSION < "$PACKAGE_VERSION_FILE" || true

if [[ ! "$PACKAGE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "invalid package version in $PACKAGE_VERSION_FILE: $PACKAGE_VERSION" >&2
    exit 1
fi

# Where a package build does its work.
#
# Deliberately not `${TMPDIR:-/tmp}`. On a systemd machine /tmp is a tmpfs sized
# at a fraction of RAM, and building there means building in memory: a release
# build of this dependency graph writes about 1.7 GiB, and the `cargo test`
# that makepkg's check() and rpmbuild's %check run adds roughly 5 GiB more in
# the dev profile. That is how a package build ends in "No space left on
# device" — or, on a tmpfs mounted with quotas, "Disk quota exceeded" — long
# after the point where anything could be done about it.
#
# The default is beside the artifacts, on whatever filesystem the checkout is
# on, which is the one place already known to hold a build of this project.
PACKAGE_WORK_ROOT="${LXB_WORK_DIR:-$PACKAGING_DIR/out/build}"

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

# require_rust_version [MINIMUM] [HINT]
#
# HINT is a line added to the refusal, for a builder that knows how its own
# distribution gets a newer Rust.
require_rust_version() {
    local minimum="${1:-1.89}"
    local hint="${2:-}"
    local actual
    local first

    require_command rustc
    require_command cargo
    # Empty when rustc does not answer, which is how rustup reads before a
    # toolchain has been chosen — and `|| true` because under the builders'
    # pipefail that failure would otherwise end the script before a word is said.
    actual="$(rustc --version 2>/dev/null | awk '{print $2}')" || true
    first="$(printf '%s\n%s\n' "$minimum" "$actual" | sort -V | head -n 1)"
    if [[ "$first" != "$minimum" ]]; then
        package_die "Rust $minimum or newer is required by the locked dependency graph (found ${actual:-none})${hint:+
$hint}"
    fi
}

# Resolve `.` and `..` so that the guard in package_work_dir compares places
# rather than spellings: `--work-dir ../build` names somewhere outside the
# checkout, and against the raw string it still begins with $PROJECT_ROOT.
#
# Lexically, and deliberately not with `realpath`: PROJECT_ROOT and PACKAGING_DIR
# come from `cd`+`pwd`, which keeps whatever symlinks the caller walked through.
# Resolving them on one side of that comparison and not the other is how the
# same directory gets two spellings and the guard reads the wrong one.
package_normalize_path() {
    local path="$1"
    local rest="$path"
    local out=""
    local segment

    while [[ -n "$rest" ]]; do
        segment="${rest%%/*}"
        if [[ "$rest" == */* ]]; then
            rest="${rest#*/}"
        else
            rest=""
        fi
        case "$segment" in
            '' | .) ;;
            # `..` above the root stays at the root, as the kernel does it.
            ..) out="${out%/*}" ;;
            *) out="$out/$segment" ;;
        esac
    done

    printf '%s\n' "${out:-/}"
}

package_set_work_root() {
    local root="$1"

    [[ -n "$root" ]] || package_die "--work-dir requires a value"
    if [[ "$root" != /* ]]; then
        root="$PWD/$root"
    fi
    PACKAGE_WORK_ROOT="$(package_normalize_path "$root")"
}

# A relative LXB_WORK_DIR has to become absolute here rather than at first use:
# the guard in package_work_dir asks whether the work root is inside the
# checkout, and a relative path would answer no to that question however far
# inside it actually is.
if [[ -n "${LXB_WORK_DIR:-}" ]]; then
    package_set_work_root "$LXB_WORK_DIR"
fi

# A private directory under the work root, named so the cleanup traps can
# recognise their own before removing anything.
package_work_dir() {
    local slug="$1"

    # `snapshot_source` takes tracked files *plus* anything untracked and not
    # ignored, which is what lets a local build package the tree a developer is
    # actually testing. A build directory inside the checkout and outside
    # packaging/out — which is both ignored and skipped by name — would be
    # swept up by that: a source archive containing its own build tree, growing
    # each time it is built.
    # `out"*` rather than `out"/*` would also accept packaging/outtakes, which
    # snapshot_source skips by exact name and would therefore sweep up.
    if [[ "$PACKAGE_WORK_ROOT" == "$PROJECT_ROOT"/* \
        && "$PACKAGE_WORK_ROOT" != "$PACKAGING_DIR/out" \
        && "$PACKAGE_WORK_ROOT" != "$PACKAGING_DIR/out"/* ]]; then
        package_die "the build directory must be outside the checkout or under packaging/out: $PACKAGE_WORK_ROOT"
    fi

    mkdir -p "$PACKAGE_WORK_ROOT" \
        || package_die "could not create the build directory $PACKAGE_WORK_ROOT"
    mktemp -d "$PACKAGE_WORK_ROOT/$slug.XXXXXX" \
        || package_die "could not create a build directory under $PACKAGE_WORK_ROOT"
}

# Refuse a build that cannot finish, while it still costs nothing to refuse.
#
# `df` reports what the filesystem has, which is not always what this user may
# take: a quota is invisible here and will still stop the build. So this catches
# the common case and never promises the uncommon one.
require_free_space() {
    local directory="$1"
    local needed_mib="$2"
    local available

    available="$(df -Pm "$directory" 2>/dev/null | awk 'NR == 2 { print $4 }')"
    if [[ ! "$available" =~ ^[0-9]+$ ]]; then
        # Not knowing is not a reason to refuse; the build will say so itself.
        return 0
    fi
    if ((available < needed_mib)); then
        package_die "$directory has ${available} MiB free but the build needs about ${needed_mib} MiB.
Point the build somewhere with room using --work-dir DIR (or LXB_WORK_DIR),
or skip the test phase, which is most of that space."
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

    # The file list below comes from Git, so a tree Git cannot read is not one
    # this can package — and it has to be refused here, because nothing
    # downstream will refuse it. An empty file list makes `tar -T -` write an
    # empty archive and report success, and the empty source directory that
    # unpacks from it sits *below* the checkout it was meant to be a copy of.
    # So Cargo walks up out of it, finds the very workspace it should have been
    # handed, and vendors and builds that instead. Every phase that asks Cargo a
    # question passes. The first one to name a path of its own — %install
    # reaching for packaging/install.sh — is where it comes apart, several
    # gigabytes and one whole compile later.
    git -C "$PROJECT_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
        || package_die "$PROJECT_ROOT is not a Git checkout, and the source archive is built from what Git lists.
Clone the repository instead of unpacking a source download, or make this tree
one with: git init && git add -A && git commit -m 'local tree'"
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

    # And say so if it did not arrive. The pipeline above reports success for an
    # empty archive, and an empty snapshot is the one failure that goes on to
    # look like a working build.
    for required in Cargo.toml packaging/install.sh; do
        [[ -f "$destination/$required" ]] \
            || package_die "the source snapshot is missing $required: $destination"
    done
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
