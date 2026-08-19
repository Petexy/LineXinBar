#!/usr/bin/env bash

# Set the version this project releases under.
#
# `VERSION` at the root of the checkout is that number, and almost everything
# reads it where it stands: the Arch, Debian and Nix definitions, the source
# archive's name, and — through the build scripts in crates/lxb-compositor and
# crates/lxb-desktop — the two binaries, which refuse to build against a
# manifest that has drifted away from it.
#
# Two places cannot read a file and carry the number as a literal instead.
# Cargo's manifest is one: `--version` compiles in `CARGO_PKG_VERSION`, and a
# `[workspace.package] version` is where that comes from. The Fedora spec is
# the other: `Version:` has to be a literal for the spec to be a spec anyone
# could submit. This writes all three, which is what makes a release one
# command rather than three edits that have to agree.

set -euo pipefail

scripts_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd "$scripts_dir/.." && pwd)"
version_file="$project_root/VERSION"
manifest="$project_root/Cargo.toml"
spec="$project_root/packaging/fedora/lxb-desktop.spec"

die() {
    echo "error: $*" >&2
    exit 1
}

note() {
    echo "==> $*"
}

usage() {
    cat <<'USAGE'
Usage: scripts/bump-version.sh X.Y.Z

Writes the version into VERSION, [workspace.package] in Cargo.toml, Cargo.lock
and packaging/fedora/lxb-desktop.spec. Every other package definition reads
VERSION for itself.

Run with the version already in VERSION to write the other three back into
agreement with it.
USAGE
}

case "${1:-}" in
    -h | --help)
        usage
        exit 0
        ;;
    "") usage >&2; exit 1 ;;
esac

new_version="$1"
shift
[[ $# -eq 0 ]] || die "unexpected argument: $1"

# The same shape packaging/lib.sh insists on when it reads the file back, and
# the shape Cargo and RPM both accept without interpretation.
[[ "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "a version is three numbers separated by dots, not: $new_version"

for required in "$version_file" "$manifest" "$spec"; do
    [[ -f "$required" ]] || die "not there: $required"
done
IFS= read -r old_version < "$version_file" || true

if [[ "$old_version" == "$new_version" ]]; then
    note "VERSION already says $new_version; writing the rest into agreement with it"
else
    note "$old_version -> $new_version"
fi

# Every write lands in a temporary beside its target and is moved into place
# afterwards. An edit this script rejects — a manifest whose table it could not
# find, a spec whose field it could not set — leaves the file it rejected
# untouched rather than half written, and there is no version of this that ends
# with three files disagreeing.
#
# And the temporaries go away with it. A source archive is built from tracked
# files plus everything untracked and not ignored, so a `Cargo.toml.new` left
# behind by a rejected edit would not sit there quietly: it would ship.
cleanup() {
    rm -f -- "$version_file.new" "$manifest.new" "$spec.new"
}
trap cleanup EXIT

printf '%s\n' "$new_version" > "$version_file.new"

# `version` appears under several tables in this manifest — every dependency
# with a version requirement has one — so the edit is anchored to the table it
# belongs to rather than to the first line that looks right. This is the same
# parse packaging/check.sh reads the version back with.
awk -v version="$new_version" '
    /^\[/ { section = $0 }
    section == "[workspace.package]" && !replaced \
        && /^version[[:space:]]*=[[:space:]]*"[^"]*"[[:space:]]*$/ {
        print "version = \"" version "\""
        replaced = 1
        next
    }
    { print }
    END { if (!replaced) exit 1 }
' "$manifest" > "$manifest.new" \
    || die "could not find [workspace.package] version in $manifest"

# Keep the existing column alignment: the field is one of a block of headers a
# spec lines up, and rewriting the whitespace would show up as noise in every
# release diff.
sed -E "s/^(Version:[[:space:]]+).*\$/\1$new_version/" "$spec" > "$spec.new"
grep -Eq "^Version:[[:space:]]+${new_version//./\\.}\$" "$spec.new" \
    || die "could not set Version: in $spec"

# Nothing above this line has touched a file anyone reads. Everything below it
# is a rename.
mv -- "$version_file.new" "$version_file"
mv -- "$manifest.new" "$manifest"
mv -- "$spec.new" "$spec"

# Cargo.lock carries a version for each workspace member, and every packaged
# build is `--locked` or `--frozen`: a lock file left behind is not a stale
# number somewhere, it is a build that refuses to start.
command -v cargo >/dev/null 2>&1 \
    || die "cargo is needed to update Cargo.lock; it is otherwise done"
cargo update --manifest-path "$manifest" --workspace --offline --quiet \
    || die "could not update Cargo.lock; run: cargo update --workspace"

note "wrote VERSION, Cargo.toml, Cargo.lock and the Fedora spec"
cat <<NEXT

Still by hand:
  * a %changelog entry for $new_version-1 in packaging/fedora/lxb-desktop.spec,
    which a spec needs and only a person can write.

Then: ./packaging/build.sh check
NEXT
