#!/usr/bin/env bash
# Installs (or upgrades) PlasmaTrack for this user.
#
#   ./install.sh [--server-url URL]
#
# Asks for the server URL and your player token. Set PLASMATRACK_TOKEN to give
# the token without a prompt (never as an argument: it would show in the process
# list and your shell history). On an upgrade, press Enter to keep each value.
#
# Everything that touches the config or game folders is done by
# plasmatrack-client itself.
set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
warn() { printf 'warning: %s\n' "$*" >&2; }
json_string() { local s=${1//\\/\\\\}; printf '"%s"' "${s//\"/\\\"}"; }

main() {
    local server_url="" token="${PLASMATRACK_TOKEN:-}"
    while (($#)); do
        case $1 in
            --server-url) server_url=${2:?--server-url needs a URL}; shift 2 ;;
            -h | --help) sed -n '2,12p' "$0"; return 0 ;;
            *) die "unknown argument: $1" ;;
        esac
    done
    ((EUID != 0)) || die "run this as your own user, not with sudo"
    [[ $(uname -m) == x86_64 ]] || die "this package is for x86_64 PCs"

    local here data bin client unit_dir unit
    here=$(cd "$(dirname "$0")" && pwd)
    data=${XDG_DATA_HOME:-$HOME/.local/share}/plasmatrack
    bin=$data/bin
    client=$bin/plasmatrack-client
    unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
    unit=$unit_dir/plasmatrack-client.service
    for f in plasmatrack-client plasmatrack.dll plasmatrack-client.service uninstall.sh; do
        [[ -f $here/$f ]] || die "$f is missing; extract the whole package and run install.sh from it"
    done

    # What is already set, so an upgrade need not ask again.
    # A config an older version wrote may no longer load; set-local below still
    # keeps its [local] values.
    local shown old_url="" has_token=unknown
    if shown=$("$here/plasmatrack-client" show-local 2>/dev/null); then
        old_url=$(sed -n 's/.*"server_url": "\(.*\)",\{0,1\}$/\1/p' <<<"$shown")
        [[ $old_url == *.invalid* ]] && old_url=""
        has_token=no
        grep -q '"token_set": true' <<<"$shown" && has_token=yes
    fi

    if [[ -z $server_url ]]; then
        read -rp "Server URL${old_url:+ [$old_url]}: " server_url || true
        server_url=${server_url:-$old_url}
    fi
    [[ -n $server_url || $has_token == unknown ]] || die "a server URL is required (ask your PlasmaTrack admin)"
    if [[ -z $token ]]; then
        local hint="from /token new on Discord"
        [[ $has_token == no ]] || hint="Enter keeps the current one"
        read -rsp "Player token ($hint): " token || true
        echo
    fi

    # A running client holds the ports and the binary.
    if command -v systemctl >/dev/null; then
        systemctl --user stop plasmatrack-client.service 2>/dev/null || true
    fi
    "$here/plasmatrack-client" stop >/dev/null 2>&1 || true

    mkdir -p "$bin"
    install -m755 "$here/plasmatrack-client" "$client.new"
    mv -f "$client.new" "$client"
    install -m644 "$here/plasmatrack.dll" "$bin/plasmatrack.dll"
    install -m755 "$here/uninstall.sh" "$bin/uninstall.sh"

    # The config this build ships, with your [local] values, readable only by you.
    local fields=()
    [[ -z $server_url ]] || fields+=("\"server_url\": $(json_string "$server_url")")
    [[ -z $token ]] || fields+=("\"token\": $(json_string "$token")")
    (IFS=, && printf '{%s}' "${fields[*]-}") | "$client" set-local
    [[ -n $token || $has_token != no ]] || warn "no token yet: jobs are kept on this PC until you run install.sh again with one"

    local failed=0 games
    games=$("$client" find-games)
    if [[ -z $games ]]; then
        warn "no ETS2 or ATS install found; start the game once from Steam, then run install.sh again"
    else
        "$client" install-plugin --plugin "$bin/plasmatrack.dll" || failed=1
        if [[ -f $here/plasmatrack.so ]]; then
            "$client" install-plugin --plugin "$here/plasmatrack.so" || failed=1
        elif grep -q '/bin/linux_x64/' <<<"$games"; then
            warn "a native Linux game install was found; PlasmaTrack needs the game run with Proton (Steam > game > Properties > Compatibility), then run install.sh again"
        fi
    fi

    mkdir -p "$unit_dir"
    local new_unit
    new_unit=$(<"$here/plasmatrack-client.service")
    new_unit=${new_unit//'%h/.local/share/plasmatrack/bin'/"$bin"}
    if [[ -f $unit && $(<"$unit") != "$new_unit" ]]; then
        cp "$unit" "$unit.bak"
        warn "replaced your plasmatrack-client.service; the old one is at $unit.bak"
    fi
    printf '%s\n' "$new_unit" >"$unit"
    if command -v systemctl >/dev/null &&
        systemctl --user daemon-reload &&
        systemctl --user enable plasmatrack-client.service &&
        systemctl --user restart plasmatrack-client.service; then
        echo "PlasmaTrack is running and starts when you log in."
    else
        warn "could not start the systemd user service; start PlasmaTrack yourself with: $client run"
    fi

    ((failed == 0)) || die "the plugin could not be installed in every game (see above); close the games and run install.sh again"
    echo "Done. To remove PlasmaTrack: $bin/uninstall.sh"
}

main "$@"
