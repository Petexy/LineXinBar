#!/usr/bin/env bash
#
# A runtime directory for a nested session, and the reason there has to be one.
# Sourced by run-nested.sh and nested-shot.sh; not an entry point on its own.
#
# Both of those put the nested session on a private bus with `dbus-run-session`,
# so that D-Bus activation cannot escape to the outer desktop. That bus is not
# wired to systemd, so a service file's `SystemdService=` line means nothing to
# it and it falls back to `Exec=`. Ask it for `org.freedesktop.portal.Documents`
# — which every flatpak started inside does, and so does every file dialog — and
# it runs a *second* `/usr/lib/xdg-document-portal`.
#
# That second portal reads `XDG_RUNTIME_DIR`, finds the host's, and mounts its
# FUSE filesystem over `/run/user/$UID/doc`: the path the real session's portal
# already owns. When the nested session ends, it unmounts it. From then on
# nothing on the machine will start —
#
#     bwrap: Can't find source path /run/user/1003/doc/by-app/<app id>
#
# — with the host's portal still running, `systemctl status` reporting it
# active, and its mount simply gone. It stays that way until somebody restarts
# xdg-document-portal, and nothing anywhere says why. Fixing it is
# `systemctl --user restart xdg-document-portal`.

# Make a runtime directory for a nested session and print its path.
#
# It is a view of the host's: a symlink for every socket already there, so an
# activated service still finds Wayland and PipeWire, with `doc` left out
# because that is the one name being fought over. Prints nothing and fails if
# one cannot be made, which a caller should treat as "carry on without it".
#
# `pulse` is the one name that has to be a real directory rather than a link to
# one. libpulse will not use a socket until it has opened the directory holding
# it with `O_NOFOLLOW` — `pa_make_secure_dir`, which is there so that nobody
# else can leave a socket in your runtime directory — and a symlink answers that
# open with `ELOOP`:
#
#     Failed to create secure directory (…/pulse): Too many levels of symbolic
#     links
#
# Everything started inside a nested session gets this directory, so everything
# in one that plays through libpulse got that sentence instead of sound, and
# `pactl` run in there exited 1 with nothing at all on stdout. It is easy to
# miss because the two programs that do *not* go through libpulse still work:
# `wpctl` and anything speaking PipeWire's own protocol reach the server through
# the `pipewire-0` socket, which no such check is made on — so the session has
# sound, has a volume bar, and cannot list a single stream.
#
# So the directory is made for real, 0700 as libpulse insists, and the socket,
# the cookie and the pid file inside it are linked one at a time.
nested_runtime_make() {
    local host="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    local made entry inner name

    made="$(mktemp -d "$host/lxb-nested-XXXXXX" 2>/dev/null)" || return 1
    chmod 700 "$made"
    for entry in "$host"/*; do
        name="${entry##*/}"
        case "$name" in doc | lxb-nested-*) continue ;; esac
        if [[ "$name" == pulse && -d "$entry" ]]; then
            mkdir -m 700 "$made/pulse" 2>/dev/null || true
            for inner in "$entry"/*; do
                [[ -e "$inner" ]] || continue
                ln -s "$inner" "$made/pulse/${inner##*/}" 2>/dev/null || true
            done
            continue
        fi
        [[ -e "$made/$name" ]] || ln -s "$entry" "$made/$name" 2>/dev/null || true
    done
    printf '%s\n' "$made"
}

# Hand the directory to everything the session bus in *this* environment
# activates.
#
# Only ever on a private bus. On the host's own bus this would reach into the
# desktop the developer is sitting in front of, which is the opposite of the
# point.
#
# `--print-reply` is here to wait, not to print: without it dbus-send returns
# before the daemon has taken the new environment and the first activation
# races it. That race is silent and looks exactly like the bug this is for.
nested_runtime_tell_the_bus() {
    local runtime="$1"

    [[ -n "$runtime" ]] || return 0
    command -v dbus-send >/dev/null 2>&1 || {
        echo "warning: dbus-send is unavailable; a nested document portal may unmount the host's" >&2
        return 0
    }
    dbus-send --print-reply --session --dest=org.freedesktop.DBus \
        /org/freedesktop/DBus org.freedesktop.DBus.UpdateActivationEnvironment \
        dict:string:string:"XDG_RUNTIME_DIR","$runtime" >/dev/null
}

# Take the directory down again.
#
# A document portal started inside may still have its filesystem mounted in
# here — the private bus outlives the script that started it — so that comes
# down first, or `rm` walks into a live FUSE mount. Best effort throughout: the
# directory is on a tmpfs and goes with the session either way.
nested_runtime_drop() {
    local runtime="$1"

    [[ -n "$runtime" && "$runtime" == */lxb-nested-* ]] || return 0
    if [[ -d "$runtime/doc" ]] && command -v fusermount3 >/dev/null 2>&1; then
        fusermount3 -u "$runtime/doc" 2>/dev/null || true
    fi
    rm -rf -- "$runtime" 2>/dev/null || true
}