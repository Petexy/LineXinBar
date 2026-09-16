#!/usr/bin/env bash
#
# Photograph Settings > Updates in a nested session that never reaches a real
# package manager, fwupd or the network.
#
# The fake tools in scripts/updates-fixture/bin are put first on PATH for the
# compositor, the shell and a test-only coordinator, so the page is
# driven by a system with updates, two Flatpak scopes and three fwupd devices
# of which one is eligible. A root step cannot be faked — the
# coordinator hands pkexec only a root-owned executable — so the system's
# step is refused before pkexec is reached, and that refusal is what the
# system source shows in these pictures. See scripts/updates-fixture/README.md.
#
# The session runs on a rootful Xwayland of its own with the pointer parked in
# its corner, so the user's own mouse cannot move the highlight. Afterwards the
# test coordinator is stopped. No installed helper or host service is changed.
#
# Usage:
#   scripts/updates-shot.sh OUT_DIR "6:left,…,11:launch,14:launch,17:screenshot"
#   scripts/updates-shot.sh OUT_DIR "…" 'sleep 20; pkcheck … --process $(pgrep -n lxb-desktop) -u'
#
# The third argument, as nested-shot.sh's, is a command run inside the
# session a few seconds after it starts — the way to ask the nested shell's
# own polkit agent something while the updates panel is up.
#
# The walk to the page: from the start screen, `left` until the category row
# clamps on Settings, `launch` into Updates (its first row), then `launch` on
# a row of the page. Every call the fakes receive is written to
# OUT_DIR/home/fixture-calls.log — under the scratch HOME, because that is one
# of the few variables the coordinator's transient unit is handed.

set -euo pipefail

out="${1:?usage: updates-shot.sh OUT_DIR ACTIONS [COMMAND]}"
actions="${2:?usage: updates-shot.sh OUT_DIR ACTIONS [COMMAND]}"
inside="${3:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$(realpath -m -- "$out")"
mkdir -p "$out"
display="${LXB_SHOT_DISPLAY:-:7}"

# The production helper now refuses installation without real power protection
# and a protected, installed authorization worker. Use the integration test's
# injected runtime for screenshots; never add a bypass to the shipped helper.
fixture=$(cd "$root" && cargo test -p lxb-updates --test coordinator --no-run --offline --message-format=json \
    | python3 -c 'import json,sys; print(next(x["executable"] for x in map(json.loads,sys.stdin) if x.get("executable") and x.get("target",{}).get("name")=="coordinator"))')
export PATH="$root/scripts/updates-fixture/bin:/usr/bin:/bin"
export RUST_LOG="${RUST_LOG:-lxb_desktop=debug,lxb_updates=debug}"
export XDG_STATE_HOME="$out/home/.local/state"
host_runtime="${XDG_RUNTIME_DIR:-}"
export XDG_RUNTIME_DIR="$out/runtime"
mkdir -p "$out/home" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
rm -f "$out/home/fixture-calls.log"
HOME="$out/home" LXB_FIXTURE_DAEMON=1 "$fixture" --exact fixture_daemon --nocapture > "$out/coordinator.log" 2>&1 &
coordinator=$!
xwayland=
parked=
cleanup() {
    [[ -z "$parked" ]] || kill "$parked" 2>/dev/null || true
    [[ -z "$xwayland" ]] || kill "$xwayland" 2>/dev/null || true
    kill "$coordinator" 2>/dev/null || true
    wait "$coordinator" 2>/dev/null || true
}
trap cleanup EXIT
XDG_RUNTIME_DIR="$host_runtime" Xwayland "$display" -geometry "${LXB_SHOT_SIZE:-1280x800}" -noreset > "$out/xwayland.log" 2>&1 &
xwayland=$!
sleep 2
python3 "$root/scripts/park-pointer.py" "$display" 600 > /dev/null 2>&1 &
parked=$!

status=0
LXB_SHOT_DISPLAY="$display" LXB_SHOT_ARGS="--no-gamepad --no-steam ${LXB_SHOT_ARGS:-}" \
    "$root/scripts/nested-shot.sh" "$out" "$actions" "$inside" || status=$?

exit "$status"
