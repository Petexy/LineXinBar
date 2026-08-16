#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$script_dir/../lib.sh"

output_dir="$PACKAGING_DIR/out/arch"
allow_foreign=false
makepkg_extra=()

usage() {
    cat <<'EOF'
Usage: packaging/arch/build.sh [OPTIONS] [-- MAKEPKG OPTIONS]

Options:
  --output-dir DIR       Artifact directory (default: packaging/out/arch)
  --work-dir DIR         Where makepkg builds (default: packaging/out/build)
  --allow-foreign-host   Permit a test build on an Arch-derived non-Arch host

The build is not run under /tmp: that is a tmpfs on most machines, and this
dependency graph needs about 1.7 GiB to compile plus roughly 5 GiB more for
the dev-profile build that check() runs. LXB_WORK_DIR sets the same thing.

Examples:
  packaging/arch/build.sh
  packaging/arch/build.sh -- --syncdeps
  packaging/arch/build.sh -- --nocheck
  packaging/arch/build.sh --work-dir /var/tmp/linexinbar
EOF
}

while (($#)); do
    case "$1" in
        --output-dir)
            (($# >= 2)) || package_die "--output-dir requires a value"
            output_dir="$2"
            shift 2
            ;;
        --work-dir)
            (($# >= 2)) || package_die "--work-dir requires a value"
            package_set_work_root "$2"
            shift 2
            ;;
        --allow-foreign-host) allow_foreign=true; shift ;;
        --) shift; makepkg_extra=("$@"); break ;;
        -h|--help) usage; exit 0 ;;
        *) package_die "unknown Arch builder option: $1" ;;
    esac
done

if [[ "$allow_foreign" != true ]] && ! host_is_like arch; then
    package_die "build Arch packages on Arch Linux or pass --allow-foreign-host for a derivative"
fi
if [[ "$output_dir" != /* ]]; then
    output_dir="$PWD/$output_dir"
fi

require_command makepkg
require_command sha256sum
require_rust_version 1.89

work="$(package_work_dir linexinbar-arch)"
cleanup() {
    if [[ -n "${work:-}" && "$work" == */linexinbar-arch.* && -d "$work" ]]; then
        rm -rf -- "$work"
    fi
}
trap cleanup EXIT

# check() builds the whole graph a second time in the dev profile, which is
# most of this number. Ask for it before extracting anything, so a machine
# without the room is told now rather than after the first thousand crates.
if [[ " ${makepkg_extra[*]} " == *" --nocheck "* ]]; then
    require_free_space "$work" 3072
else
    require_free_space "$work" 10240
fi

# makepkg.conf wins over the environment, so a machine that has set BUILDDIR
# builds there whatever this script was told. Say so rather than let the build
# fail somewhere this script was careful not to put it.
if [[ -n "$(. /etc/makepkg.conf 2>/dev/null; printf '%s' "${BUILDDIR:-}")" ]]; then
    package_note "note: makepkg.conf sets BUILDDIR; makepkg builds there, not in $work"
fi

source_dir="$work/linexinbar-$PACKAGE_VERSION"
source_archive="$work/linexinbar-$PACKAGE_VERSION.tar.gz"
snapshot_source "$source_dir"
archive_snapshot "$source_dir" "$source_archive"
checksum="$(sha256sum "$source_archive" | awk '{print $1}')"
sed -e "s/@SOURCE_SHA256@/$checksum/" -e "s/@VERSION@/$PACKAGE_VERSION/" \
    "$script_dir/PKGBUILD.in" > "$work/PKGBUILD"

package_note "building the Arch package"
(
    cd "$work"
    makepkg --cleanbuild --clean --force "${makepkg_extra[@]}"
)

mkdir -p "$output_dir"
# Ask makepkg where it put things rather than looking beside the PKGBUILD: a
# makepkg.conf that sets PKGDEST writes elsewhere, and a search of this
# directory would then quietly collect nothing. `--packagelist` also names the
# debug package when makepkg.conf asks for one.
collected=0
while IFS= read -r artifact; do
    [[ -f "$artifact" ]] || continue
    install -m0644 "$artifact" "$output_dir/$(basename "$artifact")"
    collected=$((collected + 1))
done < <(cd "$work" && makepkg --packagelist)
((collected > 0)) || package_die "makepkg reported no package files to collect"

package_note "created $collected Arch artifact(s) in $output_dir"
