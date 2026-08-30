#!/usr/bin/env bash
#
# Make a build in this checkout the folder handler for LineXinBar sessions run
# from it.
#
# Most of "Show in folder" needs nothing installed. The shell takes
# `org.freedesktop.FileManager1` on the session bus while it runs, and that is
# the name a browser, an archiver and `xdg-desktop-portal`'s `OpenDirectory` all
# call — a name already owned is never activated, so Dolphin is never started
# behind it. See `crates/lxb-desktop/src/reveal.rs`.
#
# What does need installing is the other road to the same place: `xdg-open` on a
# folder, and every application that falls back to launching whatever the
# machine says opens one. That is a desktop entry claiming `inode/directory` and
# a list naming it as the default, and without them a checkout session opens
# whatever file manager the machine already had.
#
# The list is deliberately desktop-specific — `linexinbar-mimeapps.list` is read
# only while `XDG_CURRENT_DESKTOP` lowercases to `linexinbar` — so this takes
# folders away from nothing else on the machine and never writes into the user's
# own `mimeapps.list`.
#
# A packaged LineXinBar installs both into /usr (see packaging/install.sh). This
# is for a session run straight out of a git checkout, which installs nothing:
# the same two files, in the user's own data directory, where everything that
# reads them looks before it looks in /usr.
#
# Usage:
#   scripts/install-file-manager.sh              # register the build in ./target/release
#   scripts/install-file-manager.sh --uninstall  # take the registration away again

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
entry_file="$data_home/applications/linexinbar-files.desktop"
defaults_file="$data_home/applications/linexinbar-mimeapps.list"

if [[ "${1:-}" == "--uninstall" ]]; then
    rm -f "$entry_file" "$defaults_file"
    echo "removed the folder handler from $data_home"
    exit 0
fi

if [[ $# -gt 0 ]]; then
    echo "usage: ${BASH_SOURCE[0]} [--uninstall]" >&2
    exit 2
fi

binary="$root/target/release/lxb-desktop"
[[ -x "$binary" ]] || {
    echo "no shell to register at $binary — run: cargo build --release" >&2
    exit 1
}

# The defaults list travels as it is: it names a desktop entry by file name,
# which is the same wherever that entry was installed.
install -Dm0644 "$root/share/applications/linexinbar-mimeapps.list" "$defaults_file"

# The entry is the one that cannot be copied as it stands: it names the binary
# to run, and a checkout's is not the packaged /usr/bin path. An entry pointing
# at a binary that is not there is a folder press that fails silently, which is
# worse than no entry at all.
install -Dm0644 "$root/share/applications/linexinbar-files.desktop" "$entry_file"
sed -i \
    -e "s|^Exec=lxb-desktop |Exec=$binary |" \
    -e "s|^TryExec=lxb-desktop$|TryExec=$binary|" \
    "$entry_file"

echo "registered $binary as the folder handler for this user:"
echo "  $entry_file"
echo "  $defaults_file"
echo
echo "It has a say only inside a LineXinBar session, which is what sets"
echo "XDG_CURRENT_DESKTOP=LineXinBar. Some programs cache the desktop entries"
echo "they know about, so update the index if one of them does not notice:"
echo "  update-desktop-database \"$data_home/applications\""
