#!/usr/bin/env bash
#
# Start LineXinBar nested inside the current session with the XMB shell, for
# development. Both processes are killed when this script exits.
#
# Usage:
#   scripts/run-nested.sh              # one virtual display (winit)
#   scripts/run-nested.sh 3            # three virtual displays (X11 backend)

set -euo pipefail

outputs="${1:-1}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
socket="lxb-dev-$$"
host_wayland_display="${LXB_HOST_WAYLAND_DISPLAY:-${WAYLAND_DISPLAY:-}}"
host_wayland_socket="${LXB_HOST_WAYLAND_SOCKET:-${WAYLAND_SOCKET:-}}"
host_display="${LXB_HOST_DISPLAY:-${DISPLAY:-}}"

# A host session bus can redirect single-instance/D-Bus-activated applications
# back to a process on the outer desktop even when WAYLAND_DISPLAY is private.
# Put the whole nested session on its own bus so activation inherits LineXinBar's
# display boundary. The marker prevents recursion after dbus-run-session execs
# this script again.
if [[ "${LXB_PRIVATE_DBUS:-0}" != 1 ]]; then
    if command -v dbus-run-session >/dev/null 2>&1; then
        # The bus daemon snapshots its activation environment at startup.
        # Keep the host displays out of that snapshot, while retaining them
        # under private names so the compositor itself can still create its
        # nested host window.
        exec env \
            -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u DISPLAY \
            LXB_PRIVATE_DBUS=1 \
            LXB_HOST_WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" \
            LXB_HOST_WAYLAND_SOCKET="${WAYLAND_SOCKET:-}" \
            LXB_HOST_DISPLAY="${DISPLAY:-}" \
            dbus-run-session -- "$root/scripts/run-nested.sh" "$@"
    fi
    echo "warning: dbus-run-session is unavailable; host D-Bus activation may escape LineXinBar" >&2
fi

cargo build --release --locked --manifest-path "$root/Cargo.toml"

if [[ "$outputs" -gt 1 ]]; then
    # Only the X11 backend can open more than one window.
    args=(--backend x11 --outputs "$outputs" --window-size 960x600)
else
    args=(--backend winit)
fi

# `--shell` starts lxb-desktop as the session shell: the compositor's own
# spawn path supplies the private Wayland/XWayland display names once those
# servers are ready, which an outer script cannot do, and quitting the shell
# ends the session rather than leaving an empty compositor behind.
#
# Restore the outer display only for LineXinBar's host-window backend. Its child
# launch path replaces these values with the private Wayland/XWayland names.
host_env=(env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u DISPLAY)
if [[ -n "$host_wayland_display" ]]; then
    host_env+=("WAYLAND_DISPLAY=$host_wayland_display")
fi
if [[ -n "$host_wayland_socket" ]]; then
    host_env+=("WAYLAND_SOCKET=$host_wayland_socket")
fi
if [[ -n "$host_display" ]]; then
    host_env+=("DISPLAY=$host_display")
fi

"${host_env[@]}" "$root/target/release/lxb" "${args[@]}" --socket "$socket" --shell &
compositor=$!

cleanup() {
    kill "$compositor" 2>/dev/null || true
    wait "$compositor" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Wait for the socket to appear rather than guessing how long startup takes.
runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
for _ in $(seq 1 100); do
    [[ -S "$runtime/$socket" ]] && break
    sleep 0.1
done

if [[ ! -S "$runtime/$socket" ]]; then
    echo "compositor did not create $runtime/$socket" >&2
    exit 1
fi

wait "$compositor"
