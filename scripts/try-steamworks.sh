#!/usr/bin/env bash
# Start one installed game through the Steamworks library in `lxb-steamworks`,
# the way the shell will, and report what happened.
#
#   scripts/try-steamworks.sh <app-id> <game-directory> <executable>
#
# The ticket must already have been fetched for this app:
#
#   cargo run -p lxb-steam --example probe-ticket -- <app-id>
#
# Nothing in the game's directory is written to. The library goes in front of
# the game's own through LD_PRELOAD, which is the same thing `DirectLaunch::
# through_steamworks` does, so what this proves is what the shell will do.

set -u

app_id=${1:?usage: try-steamworks.sh <app-id> <game-directory> <executable>}
game_dir=${2:?usage: try-steamworks.sh <app-id> <game-directory> <executable>}
program=${3:?usage: try-steamworks.sh <app-id> <game-directory> <executable>}

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
library="$repo/target/debug/libsteam_api.so"
ticket="${XDG_RUNTIME_DIR:-/tmp}/lxb/tickets/$app_id.ticket"
log=$(mktemp -t lxb-steamworks-XXXXXX.log)

# How long to let it run. A game that is still alive at the end has got past
# every Steamworks call it makes at startup, which is the whole question here.
seconds=${LXB_TRY_SECONDS:-20}

for needed in "$library" "$ticket" "$game_dir/$program"; do
	if [ ! -e "$needed" ]; then
		echo "missing: $needed" >&2
		exit 2
	fi
done

cd "$game_dir" || exit 2
LXB_STEAMWORKS_LOG=1 \
LXB_STEAMWORKS_APP_ID="$app_id" \
LXB_STEAMWORKS_TICKET="$ticket" \
LXB_STEAMWORKS_ACCOUNT="${LXB_STEAMWORKS_ACCOUNT:-Player}" \
LD_PRELOAD="$library" \
	timeout --signal=TERM "$seconds" "./$program" > "$log" 2>&1
outcome=$?

echo "=== exit $outcome after up to ${seconds}s ==="
if [ "$outcome" -eq 124 ]; then
	echo "STILL RUNNING when the clock ran out — it got through startup."
fi

# The interesting part is this library's own decisions and whatever killed the
# game. The megabyte of memory map a mono crash prints is not interesting, so
# it stays in the log rather than on the screen.
echo "--- what the library decided ---"
grep -E '^lxb-steamworks:' "$log" || echo "(it said nothing)"

echo "--- how it ended ---"
grep -E 'Stacktrace|SIGSEGV|at .*ISteam|at .*Steamworks|^Native stacktrace|libCSteamworks|libsteam_api|Unhandled|error' "$log" \
	| grep -v '^7f' | head -20 || echo "(nothing said)"

echo "--- full output kept at $log ---"
