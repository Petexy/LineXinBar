#!/usr/bin/env bash

set -euo pipefail

packaging_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
    cat <<'EOF'
Usage: packaging/build.sh TARGET [TARGET OPTIONS]

Targets:
  check    Validate package definitions and their staged payload
  debian   Build a .deb on Debian or a Debian-derived distribution
  fedora   Build binary and source RPMs on Fedora
  arch     Build an Arch package with makepkg
  nix      Build the Nix package/flake output

Each binary package must be built on its target distribution. See
docs/packaging.md for prerequisites and target-specific options.
EOF
}

target="${1:-}"
if [[ -z "$target" || "$target" == "-h" || "$target" == "--help" ]]; then
    usage
    [[ -n "$target" ]] && exit 0
    exit 2
fi
shift

case "$target" in
    check) exec "$packaging_dir/check.sh" "$@" ;;
    debian) exec "$packaging_dir/debian/build.sh" "$@" ;;
    fedora) exec "$packaging_dir/fedora/build.sh" "$@" ;;
    arch) exec "$packaging_dir/arch/build.sh" "$@" ;;
    nix) exec "$packaging_dir/nix/build.sh" "$@" ;;
    *)
        echo "unknown package target: $target" >&2
        usage >&2
        exit 2
        ;;
esac
