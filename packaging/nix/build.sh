#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=packaging/lib.sh
source "$script_dir/../lib.sh"

output_dir="$PACKAGING_DIR/out/nix"
nix_extra=()

usage() {
    cat <<'EOF'
Usage: packaging/nix/build.sh [--output-dir DIR] [-- NIX BUILD OPTIONS]

Builds the flake's linexinbar package and writes a result symlink beneath the
artifact directory. The NixOS module is exported as nixosModules.default.
EOF
}

while (($#)); do
    case "$1" in
        --output-dir)
            (($# >= 2)) || package_die "--output-dir requires a value"
            output_dir="$2"
            shift 2
            ;;
        --) shift; nix_extra=("$@"); break ;;
        -h|--help) usage; exit 0 ;;
        *) package_die "unknown Nix builder option: $1" ;;
    esac
done

if [[ "$output_dir" != /* ]]; then
    output_dir="$PWD/$output_dir"
fi
require_command nix

mkdir -p "$output_dir"
package_note "building the Nix package"
nix --extra-experimental-features 'nix-command flakes' build \
    --print-build-logs \
    --out-link "$output_dir/result" \
    "path:$PROJECT_ROOT#linexinbar" \
    "${nix_extra[@]}"
package_note "created Nix result link at $output_dir/result"
