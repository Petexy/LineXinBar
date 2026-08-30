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

work="$(package_work_dir linexinbar-debian)"
cleanup() {
    if [[ -n "${work:-}" && "$work" == */linexinbar-debian.* && -d "$work" ]]; then
        rm -rf -- "$work"
    fi
}
trap cleanup EXIT

architecture="$(dpkg --print-architecture)"
mkdir -p "$output_dir"
mkdir -p "$work/shlibs/debian"
install -m0644 "$script_dir/source-control" "$work/shlibs/debian/control"

# Build one binary package from one component of the staged tree.
#
#   build_deb COMPONENT NAME CONTROL BINARY...
#
# The binaries named are the ones stripped and handed to dpkg-shlibdeps, which
# is how each package ends up declaring only the libraries its own contents
# actually link against — the whole reason the compositor can be installed
# without the shell's audio and PipeWire stack.
build_deb() {
    local component="$1" name="$2" control="$3"
    shift 3
    local binaries=("$@")

    local package_root="$work/$name"
    "$PACKAGING_DIR/install.sh" \
        --destdir "$package_root" \
        --target-dir "$target_dir" \
        --component "$component"

    # No /usr/share/licenses here: that is the RPM and Arch convention. On
    # Debian the copyright file is the licence record, and it points at the
    # GPL-3 and Apache-2.0 texts every Debian system already carries in
    # /usr/share/common-licenses.
    install -Dm0644 "$script_dir/copyright" "$package_root/usr/share/doc/$name/copyright"

    # Staged before the package is built, so it is weighed by Installed-Size
    # and listed in md5sums. The configuration reference goes with the
    # compositor, which is what reads `config.toml`.
    case "$component" in
        compositor)
            install -Dm0644 "$PROJECT_ROOT/docs/configuration.md" \
                "$package_root/usr/share/doc/$name/configuration.md"
            install -Dm0644 "$PROJECT_ROOT/examples/config.toml" \
                "$package_root/usr/share/doc/$name/config.example.toml"
            ;;
        desktop)
            install -Dm0644 "$PROJECT_ROOT/README.md" \
                "$package_root/usr/share/doc/$name/README.md"
            ;;
    esac

    local binary
    for binary in "${binaries[@]}"; do
        [[ -f "$package_root/usr/bin/$binary" ]] \
            || package_die "$name does not contain usr/bin/$binary"
        if command -v strip >/dev/null 2>&1; then
            strip --strip-unneeded "$package_root/usr/bin/$binary"
        fi
    done

    local shlib_arguments=()
    for binary in "${binaries[@]}"; do
        shlib_arguments+=("-e$package_root/usr/bin/$binary")
    done
    local shlib_output
    shlib_output="$({
        cd "$work/shlibs"
        dpkg-shlibdeps -O "${shlib_arguments[@]}"
    })"
    [[ "$shlib_output" == shlibs:Depends=* ]] \
        || package_die "could not determine Debian shared-library dependencies for $name"
    local shlib_depends="${shlib_output#shlibs:Depends=}"

    local installed_size
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
        }' "$script_dir/$control" > "$package_root/DEBIAN/control"

    (
        cd "$package_root"
        find usr -type f -print0 | sort -z | xargs -0 md5sum
    ) > "$package_root/DEBIAN/md5sums"

    local artifact="$output_dir/${name}_${PACKAGE_VERSION}-1_${architecture}.deb"
    dpkg-deb --root-owner-group -Zxz --build "$package_root" "$artifact"
    dpkg-deb --info "$artifact" >/dev/null
    package_note "created $artifact"
}

# The compositor first: the desktop package declares a versioned dependency on
# it, and it is the half a display manager installs on its own.
build_deb compositor lxb-compositor control-compositor.in lxb
build_deb desktop lxb-desktop control.in lxb-desktop lxb-portal
# And the optional integration, which depends on the shell above for the reason
# the shell depends on the compositor: what the two agree about is a protocol,
# and a helper out of step with the shell beside it is refused rather than half
# understood.
build_deb retroarch lxb-retroarch control-retroarch.in lxb-retroarch
