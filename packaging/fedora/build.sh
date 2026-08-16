#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$script_dir/../lib.sh"

output_dir="$PACKAGING_DIR/out/fedora"
allow_foreign=false
run_checks=true
rpmbuild_extra=()

usage() {
    cat <<'EOF'
Usage: packaging/fedora/build.sh [OPTIONS] [-- RPMBUILD OPTIONS]

Options:
  --output-dir DIR       Artifact directory (default: packaging/out/fedora)
  --work-dir DIR         Where rpmbuild works (default: packaging/out/build)
  --no-check             Pass --nocheck to rpmbuild
  --allow-foreign-host   Permit a metadata/test build outside Fedora; also
                         passes --nodeps because the Fedora RPM DB is absent

The builder snapshots the current tree, vendors locked Cargo dependencies for
an offline RPM build, then produces both a source RPM and a binary RPM.

The build is not run under /tmp: that is a tmpfs on most machines, and between
the vendored dependency tree, the release build and the dev-profile build that
%check runs this needs several gigabytes. LXB_WORK_DIR sets the same thing.
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
        --no-check) run_checks=false; shift ;;
        --allow-foreign-host) allow_foreign=true; shift ;;
        --) shift; rpmbuild_extra=("$@"); break ;;
        -h|--help) usage; exit 0 ;;
        *) package_die "unknown Fedora builder option: $1" ;;
    esac
done

if [[ "$allow_foreign" != true ]] && ! host_is_like fedora; then
    package_die "build Fedora RPMs on Fedora; use --allow-foreign-host only for metadata testing"
fi
if [[ "$output_dir" != /* ]]; then
    output_dir="$PWD/$output_dir"
fi

require_command rpmbuild
require_command tar
require_command gzip
require_rust_version 1.89

work="$(package_work_dir linexinbar-fedora)"
cleanup() {
    if [[ -n "${work:-}" && "$work" == */linexinbar-fedora.* && -d "$work" ]]; then
        rm -rf -- "$work"
    fi
}
trap cleanup EXIT

# The vendored dependency tree, the release build, and — unless it was turned
# off — the whole graph built a second time in the dev profile by %check.
if [[ "$run_checks" == true ]]; then
    require_free_space "$work" 12288
else
    require_free_space "$work" 5120
fi

topdir="$work/rpmbuild"
source_dir="$work/linexinbar-$PACKAGE_VERSION"
mkdir -p "$topdir/BUILD" "$topdir/BUILDROOT" "$topdir/RPMS" \
    "$topdir/SOURCES" "$topdir/SPECS" "$topdir/SRPMS"
snapshot_source "$source_dir"

package_note "vendoring locked Rust dependencies for the offline RPM build"
mkdir -p "$source_dir/.cargo"
(
    cd "$source_dir"
    cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml
)

source_archive="$topdir/SOURCES/linexinbar-$PACKAGE_VERSION.tar.gz"
archive_snapshot "$source_dir" "$source_archive"
install -m0644 "$script_dir/lxb-desktop.spec" "$topdir/SPECS/lxb-desktop.spec"

rpmbuild_args=(-ba --define "_topdir $topdir")
[[ "$run_checks" == true ]] || rpmbuild_args+=(--nocheck)
[[ "$allow_foreign" != true ]] || rpmbuild_args+=(--nodeps)
rpmbuild_args+=("${rpmbuild_extra[@]}")

package_note "building Fedora RPMs"
rpmbuild "${rpmbuild_args[@]}" "$topdir/SPECS/lxb-desktop.spec"

mkdir -p "$output_dir"
while IFS= read -r -d '' artifact; do
    install -m0644 "$artifact" "$output_dir/$(basename "$artifact")"
done < <(find "$topdir/RPMS" "$topdir/SRPMS" -type f -name '*.rpm' -print0)

package_note "created Fedora artifacts in $output_dir"
