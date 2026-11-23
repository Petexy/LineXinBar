#!/bin/sh
# Debian's launcher passes these arguments in this order. Leave other dialogs
# to the original tools, including any question the user has not accepted.
PATH=${PATH#*:}
export PATH
if [ "${1-}" = --question ] && [ "${2-}" = '--title=Steam installer' ]; then
    case "${7-}" in
        '--text=Steam is proprietary (binary-only) software.'*)
            if mkdir -- "${0%/*}/used" 2>/dev/null; then
                exit 0
            fi
            ;;
    esac
fi
if command -v zenity >/dev/null 2>&1; then
    exec zenity "$@"
fi
exec yad "$@"
