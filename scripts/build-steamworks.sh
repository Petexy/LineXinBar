#!/usr/bin/env bash
# Build the Steamworks library under both names a game looks for.
#
#   scripts/build-steamworks.sh [--debug]
#
# A native Linux game loads `libsteam_api.so`, which Cargo names correctly on
# its own. A Windows game under Proton loads `steam_api64.dll`, and Cargo
# cannot be told to name one library differently per target — the crate is
# `steam_api`, so the cross build produces `steam_api.dll`, which is the name a
# *32-bit* game looks for. Copying it under the 64-bit name is this script's
# only reason to exist.
#
# Both end up beside each other in the same directory, because that is how they
# are installed and how the shell looks for them.

set -eu

profile=release
flags=(--release)
if [ "${1:-}" = "--debug" ]; then
	profile=debug
	flags=()
fi

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"

target=x86_64-pc-windows-gnu
if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
	echo "The Windows target is not installed. Proton games need it:" >&2
	echo "    rustup target add $target" >&2
	echo "and a MinGW toolchain (package 'mingw-w64-gcc' on Arch)." >&2
	exit 2
fi

cargo build -p lxb-steamworks "${flags[@]}"
cargo build -p lxb-steamworks "${flags[@]}" --target "$target"

into="target/$profile"
cp "target/$target/$profile/steam_api.dll" "$into/steam_api64.dll"

echo "built into $into:"
for name in libsteam_api.so steam_api64.dll; do
	printf '  %-20s %s bytes\n' "$name" "$(stat -c %s "$into/$name")"
done
