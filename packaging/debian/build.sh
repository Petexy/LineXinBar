#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$script_dir/../lib.sh"

output_dir="$PACKAGING_DIR/out/debian"
allow_foreign=false

usage() {
    cat <<'EOF'
Usage: packaging/debian/build.sh [--output-dir DIR] [--allow-foreign-host]

Builds a Debian binary package from the current working tree. A deployable
package must be built on Debian (or a Debian derivative) so its ABI and
generated shared-library dependencies match the target system.
EOF
}

while (($#)); do
    case "$1" in
        --output-dir)
            (($# >= 2)) || package_die "--output-dir requires a value"
            output_dir="$2"
            shift 2
            ;;
        --allow-foreign-host) allow_foreign=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) package_die "unknown Debian builder option: $1" ;;
    esac
done

if [[ "$allow_foreign" != true ]] && ! host_is_like debian; then
    package_die "build Debian packages on Debian/Ubuntu; use --allow-foreign-host only for metadata testing"
fi
if [[ "$output_dir" != /* ]]; then
    output_dir="$PWD/$output_dir"
fi

require_command dpkg-deb
require_command dpkg-shlibdeps
require_command dpkg
require_command md5sum
require_rust_version 1.89

target_dir="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
if [[ "$target_dir" != /* ]]; then
    target_dir="$PROJECT_ROOT/$target_dir"
fi
package_note "building LineXinBar release binaries"
RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$PROJECT_ROOT=/usr/src/linexinbar-$PACKAGE_VERSION" \
    CARGO_TARGET_DIR="$target_dir" \
    cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" \
    --release --locked --workspace --bins

work="$(mktemp -d "${TMPDIR:-/tmp}/linexinbar-debian.XXXXXX")"
cleanup() {
    if [[ -n "${work:-}" && "$work" == */linexinbar-debian.* && -d "$work" ]]; then
        rm -rf -- "$work"
    fi
}
trap cleanup EXIT

package_root="$work/root"
"$PACKAGING_DIR/install.sh" --destdir "$package_root" --target-dir "$target_dir"

install -Dm0644 "$script_dir/copyright" "$package_root/usr/share/doc/linexinbar/copyright"
install -Dm0644 "$PROJECT_ROOT/README.md" "$package_root/usr/share/doc/linexinbar/README.md"
install -Dm0644 "$PROJECT_ROOT/docs/configuration.md" \
    "$package_root/usr/share/doc/linexinbar/configuration.md"
install -Dm0644 "$PROJECT_ROOT/examples/config.toml" \
    "$package_root/usr/share/doc/linexinbar/config.example.toml"
# No /usr/share/licenses here: that is the RPM and Arch convention. On Debian
# the copyright file above is the licence record, and it points at the GPL-3
# and Apache-2.0 texts every Debian system already carries in
# /usr/share/common-licenses.

if command -v strip >/dev/null 2>&1; then
    strip --strip-unneeded "$package_root/usr/bin/lxb" "$package_root/usr/bin/lxb-desktop"
fi

mkdir -p "$work/shlibs/debian"
install -m0644 "$script_dir/source-control" "$work/shlibs/debian/control"
shlib_output="$({
    cd "$work/shlibs"
    dpkg-shlibdeps -O \
        -e"$package_root/usr/bin/lxb" \
        -e"$package_root/usr/bin/lxb-desktop"
})"
[[ "$shlib_output" == shlibs:Depends=* ]] \
    || package_die "could not determine Debian shared-library dependencies"
shlib_depends="${shlib_output#shlibs:Depends=}"

architecture="$(dpkg --print-architecture)"
installed_size="$(du -sk "$package_root" | awk '{print $1}')"
mkdir -p "$package_root/DEBIAN"
awk \
    -v version="$PACKAGE_VERSION" \
    -v architecture="$architecture" \
    -v dependencies="$shlib_depends" \
    -v installed_size="$installed_size" \
    '{
        gsub(/@VERSION@/, version)
        gsub(/@ARCH@/, architecture)
        gsub(/@SHLIB_DEPENDS@/, dependencies)
        gsub(/@INSTALLED_SIZE@/, installed_size)
        print
    }' "$script_dir/control.in" > "$package_root/DEBIAN/control"

(
    cd "$package_root"
    find usr -type f -print0 | sort -z | xargs -0 md5sum
) > "$package_root/DEBIAN/md5sums"

mkdir -p "$output_dir"
artifact="$output_dir/linexinbar_${PACKAGE_VERSION}-1_${architecture}.deb"
dpkg-deb --root-owner-group -Zxz --build "$package_root" "$artifact"
dpkg-deb --info "$artifact" >/dev/null
package_note "created $artifact"
