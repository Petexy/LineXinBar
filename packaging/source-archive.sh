#!/usr/bin/env bash

set -euo pipefail

packaging_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$packaging_dir/lib.sh"

output="${1:-$PACKAGING_DIR/out/sources/linexinbar-$PACKAGE_VERSION.tar.gz}"
if [[ "$output" != /* ]]; then
    output="$PWD/$output"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/linexinbar-source.XXXXXX")"
cleanup() {
    if [[ -n "${work:-}" && "$work" == */linexinbar-source.* && -d "$work" ]]; then
        rm -rf -- "$work"
    fi
}
trap cleanup EXIT

source_dir="$work/linexinbar-$PACKAGE_VERSION"
snapshot_source "$source_dir"
archive_snapshot "$source_dir" "$output"
package_note "created $output"
