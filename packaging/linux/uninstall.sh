#!/usr/bin/env bash
# Removes PlasmaTrack: the service, the plugin in every game and the program.
#
#   ./uninstall.sh [--delete-data]
#
# Local data, settings and the token are kept unless --delete-data is given.
set -euo pipefail

# In a function so the whole script is read before it deletes its own folder.
main() {
    local args=()
    case ${1:-} in
        "") ;;
        --delete-data) args=(--delete-data) ;;
        -h | --help) sed -n '2,7p' "$0"; return 0 ;;
        *) echo "error: unknown argument: $1" >&2; return 2 ;;
    esac

    local here client unit
    here=$(cd "$(dirname "$0")" && pwd)
    client=${XDG_DATA_HOME:-$HOME/.local/share}/plasmatrack/bin/plasmatrack-client
    [[ -x $client ]] || client=$here/plasmatrack-client
    [[ -x $client ]] || { echo "error: plasmatrack-client not found; run uninstall.sh from the downloaded package" >&2; return 1; }
    unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/plasmatrack-client.service

    if command -v systemctl >/dev/null; then
        systemctl --user disable --now plasmatrack-client.service 2>/dev/null || true
    fi
    "$client" stop >/dev/null 2>&1 || true
    local status=0
    "$client" uninstall "${args[@]}" || status=$?
    rm -f "$unit"
    command -v systemctl >/dev/null && systemctl --user daemon-reload 2>/dev/null || true
    return $status
}

main "$@"
