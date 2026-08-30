#!/usr/bin/env bash
#
# Photograph the nested shell, driving it there with a script of key presses.
#
# The picture is taken by the shell itself — `--debug-actions …:screenshot`
# goes through the compositor's own screencopy — because on a development
# machine there may be no way to grab an X window from outside: ImageMagick is
# routinely built without its X11 delegate, and neither `xwininfo` nor
# `xdotool` is a given either. The shell can always photograph itself.
#
# The whole session runs against a scratch HOME on a private bus, so nothing it
# does — the notification daemon taking a bus name, the screenshot landing in
# Pictures, the settings it writes — can touch the desktop this is run from.
#
# Usage:
#   scripts/nested-shot.sh OUT_DIR "4:guide,5.2:up,6:right,7:launch,9:screenshot"
#   scripts/nested-shot.sh OUT_DIR "…" 'notify-send hello there'
#
# The third argument, if given, is a command run inside the nested session a
# few seconds after it starts — for putting something on the screen that has
# to come from outside the shell.

set -euo pipefail

out="${1:?usage: nested-shot.sh OUT_DIR ACTIONS [COMMAND]}"
actions="${2:?usage: nested-shot.sh OUT_DIR ACTIONS [COMMAND]}"
inside="${3:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# How long the whole run lasts, which has to outlast the last action in the
# script. Taken from the script itself rather than guessed at: a run that ends
# before its own screenshot is a run that proves nothing.
last=$(printf '%s\n' "$actions" | tr ',' '\n' | cut -d: -f1 | sort -g | tail -1)
lifetime=$(printf '%.0f' "$(echo "$last + 4" | bc)")

home="$out/home"
mkdir -p "$home/.config/lxb"
rm -rf "$home/Pictures"

# **Nothing in here may be heard.** This session has a shell in it that plays
# interface sounds, and a run of this script is somebody watching a picture,
# not listening to one — a nested shell chiming into the speakers of the
# desktop it was started from is a bug report, and was one.
#
# Three doors, because the things in here do not all play the same way.
# `.asoundrc` closes ALSA, which is the shell's own route (rodio -> cpal ->
# ALSA -> the `pulse` plugin). It does *not* close libpulse or libpipewire,
# which is how anything started by the third argument would play — mpv, a GTK
# client — so those are pointed at a server that does not exist, and SDL is
# told to play into nothing.
#
# None of it touches the real session: no sink is moved and no stream is
# rerouted. This session simply has nowhere to play.
cat > "$home/.asoundrc" <<'ASOUND'
pcm.!default { type null }
ctl.!default { type null }
ASOUND
# Anything else the shell should be started with — a `--debug-*` flag, or the
# `--retroarch-helper` that stands in for an installed integration package.
# One string, appended to the command line as written.
cat > "$home/.config/lxb/config.toml" <<EOF
[general]
shell = "$root/target/release/lxb-desktop --debug-actions $actions ${LXB_SHOT_ARGS:-}"
EOF

# A socket name of this run's own, so a session left over from a previous one
# cannot make this one fail to start.
socket="lxb-shot-$$"

env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
    PULSE_SERVER=/nonexistent-lxb-harness \
    PIPEWIRE_REMOTE=nonexistent-lxb-harness \
    SDL_AUDIODRIVER=dummy \
    HOME="$home" XDG_CONFIG_HOME="$home/.config" DISPLAY="${LXB_SHOT_DISPLAY:-:1}" \
    dbus-run-session -- bash -c '
        root=$1; socket=$2; lifetime=$3; inside=$4
        "$root/target/release/lxb" --backend x11 --outputs 1 \
            --window-size "${LXB_SHOT_SIZE:-1280x800}" --socket "$socket" --shell &
        lxb=$!
        if [[ -n "$inside" ]]; then
            sleep 6
            eval "$inside" || true
        fi
        sleep "$lifetime"
        kill $lxb 2>/dev/null || true
        wait $lxb 2>/dev/null || true
    ' bash "$root" "$socket" "$lifetime" "$inside" > "$out/nested.log" 2>&1

shots=("$home"/Pictures/Screenshots/*.png)
if [[ ! -e "${shots[0]}" ]]; then
    echo "the shell took no picture; see $out/nested.log" >&2
    exit 1
fi
printf '%s\n' "${shots[@]}"
