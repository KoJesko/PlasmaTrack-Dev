#!/usr/bin/env bash
# Writes the Linux game-process match into crates/pt-client/src/updater.rs, so the
# client knows when ETS2 / ATS are running under Proton (updater and uninstaller wait
# on that; without it `games_running()` always says yes on Linux).
#
#   scripts/set-linux-process-match.sh [--ets2 NAME|pick] [--ats NAME|pick] [--install] [--no-test]
#
#   NAME     the game's process name as Linux sees it, i.e. the .exe file name
#            (default eurotrucks2.exe / amtrucks.exe). Not the window title.
#   pick     click the running game's window instead: its PID comes from the window
#            (_NET_WM_PID, via xprop; Proton games are XWayland windows), and the
#            .exe name from that process's argv[0].
#   --install  also build the release client, copy it to ~/.local/bin and restart
#              plasmatrack-client.service if it is running.
#
# Safe to re-run: the match code is written once; later runs only change the names in
# GAME_PROCESSES (which Windows uses too, so keep them the real .exe names).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
src="$root/crates/pt-client/src/updater.rs"
ets2="eurotrucks2.exe"
ats="amtrucks.exe"
install=0
test=1

usage() { sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

# Click a window, return the .exe name of the process that owns it.
pick() {
    local game=$1 pid argv0
    command -v xprop >/dev/null || { echo "pick needs xprop (xorg-xprop)" >&2; exit 1; }
    echo "Click the $game window..." >&2
    pid=$(xprop _NET_WM_PID | sed -n 's/.*= *\([0-9][0-9]*\).*/\1/p')
    [[ -n "$pid" && -r /proc/$pid/cmdline ]] || { echo "that window has no readable PID" >&2; exit 1; }
    argv0=$(tr '\0' '\n' < "/proc/$pid/cmdline" | head -n1)
    argv0=${argv0##*[/\\]}            # last component, Windows or Unix separators
    [[ -n "$argv0" ]] || argv0=$(cat "/proc/$pid/comm")
    echo "  -> pid $pid, process $argv0" >&2
    printf '%s' "$argv0"
}

while (($#)); do
    case "$1" in
        --ets2) ets2=${2:?--ets2 needs a name or pick}; shift 2 ;;
        --ats)  ats=${2:?--ats needs a name or pick}; shift 2 ;;
        --install) install=1; shift ;;
        --no-test) test=0; shift ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage 2 ;;
    esac
done
[[ "$ets2" == pick ]] && ets2=$(pick "Euro Truck Simulator 2")
[[ "$ats" == pick ]] && ats=$(pick "American Truck Simulator")

ets2=${ets2,,}; ats=${ats,,}          # the match compares lowercase
for n in "$ets2" "$ats"; do
    [[ "$n" =~ ^[a-z0-9._+-]+$ ]] || { echo "not a plain file name: '$n'" >&2; exit 1; }
done
[[ -f "$src" ]] || { echo "not found: $src" >&2; exit 1; }

python3 -I - "$src" "$ets2" "$ats" <<'PY'
import re, sys
path, ets2, ats = sys.argv[1:4]
text = open(path, encoding="utf-8").read()

# 1. The names (shared with Windows).
text, n = re.subn(r'pub const GAME_PROCESSES: \[&str; 2\] = \[[^\]]*\];',
                  f'pub const GAME_PROCESSES: [&str; 2] = ["{ets2}", "{ats}"];', text)
if n != 1:
    sys.exit("could not find the GAME_PROCESSES line")

# 2. The match itself, replacing the TODO scaffold once.
IMPL = r'''/// Linux: the same question asked of `/proc`. Under Proton the games are Wine
/// processes, so the Windows file name has to be recognised in a Linux process.
/// Per process, `/proc/<pid>/comm` is the kernel name (Wine sets it to the .exe
/// name, cut to 15 bytes: "eurotrucks2.exe" just fits) and `/proc/<pid>/cmdline`
/// is NUL-separated argv, where Wine's argv[0] is a Windows path such as
/// `Z:\home\me\...\bin\win_x64\eurotrucks2.exe` (backslashes, any letter case).
#[cfg(not(windows))]
pub fn processes(names: &[&str]) -> Option<Vec<u32>> {
    let me = std::process::id();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Some(pid) = entry.ok().and_then(|e| e.file_name().to_str()?.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let dir = Path::new("/proc").join(pid.to_string());
        // A process can exit between read_dir and these reads; it then just doesn't match.
        let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
        let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
        if process_matches(&comm, &cmdline, names) {
            found.push(pid);
        }
    }
    Some(found)
}

/// Whether a process with this `comm` and raw `cmdline` is one of `names`: by the
/// last component of argv[0] (split on `/` and `\`), or by the kernel name, which
/// is a prefix of a longer name when the kernel cut it to 15 bytes. Never by a
/// later argument, so `grep eurotrucks2.exe` is not the game.
#[cfg(not(windows))]
fn process_matches(comm: &str, cmdline: &[u8], names: &[&str]) -> bool {
    let argv0 = String::from_utf8_lossy(cmdline.split(|&b| b == 0).next().unwrap_or_default());
    let exe = argv0.rsplit(['/', '\\']).next().unwrap_or_default().to_ascii_lowercase();
    let comm = comm.trim_end_matches('\n').to_ascii_lowercase();
    names.iter().map(|n| n.to_ascii_lowercase()).any(|n| {
        (!exe.is_empty() && exe == n) || comm == n || (comm.len() == 15 && n.len() > 15 && n.starts_with(&comm))
    })
}
'''
START = "/// Linux: the same question asked of `/proc`."
END = "    let _ = found;\n    None\n}\n"
if "fn process_matches(" in text:
    print("match code already in place; names updated")
elif "// TODO(Ari): decide whether `pid` is one of `names`" in text:
    s = text.index(START)
    e = text.index(END, s) + len(END)
    text = text[:s] + IMPL + text[e:]
    print("match code written")
else:
    sys.exit("could not find the processes() TODO scaffold to replace")

# 3. Tests, once.
TEST = r'''
    #[cfg(not(windows))]
    #[test]
    fn linux_process_names_match_wine_games() {
        let games = ["eurotrucks2.exe", "amtrucks.exe"];
        let wine_argv = b"Z:\\home\\me\\Euro Truck Simulator 2\\bin\\win_x64\\eurotrucks2.exe\0-homedir\0x\0";
        assert!(process_matches("eurotrucks2.exe\n", wine_argv, &games));
        // Seen live 2026-11-15 (tinkergame + proton-cachyos): Proton's S: game drive, and
        // /proc/<pid>/exe is Wine's preloader, so only argv[0] / comm name the game.
        let seen = b"S:\\steamapps\\common\\Euro Truck Simulator 2\\bin\\win_x64\\eurotrucks2.exe\0";
        assert!(process_matches("eurotrucks2.exe\n", seen, &games));
        assert!(process_matches("wine-preloader\n", seen, &games), "argv[0] alone is enough");
        assert!(process_matches("wine64-preload\n", b"C:\\Games\\AMTRUCKS.EXE\0", &games), "argv[0], any case");
        assert!(process_matches("amtrucks.exe\n", b"", &games), "kernel name alone (cmdline unreadable)");
        assert!(!process_matches("grep\n", b"/usr/bin/grep\0eurotrucks2.exe\0", &games), "a later argument is not the process");
        assert!(!process_matches("eurotrucks2.ex\n", b"", &games), "a shorter kernel name is not a cut one");
        assert!(process_matches("plasmatrack-cli\n", b"", &["plasmatrack-client"]), "15-byte kernel names are prefixes");
        // Whatever names GAME_PROCESSES holds must match their own (cut) kernel name.
        for n in GAME_PROCESSES {
            let comm: String = n.chars().take(15).collect();
            assert!(process_matches(&format!("{comm}\n"), b"", &[n]), "{n}");
        }
    }
'''
if "fn linux_process_names_match_wine_games" not in text:
    anchor = "mod tests {\n    use super::*;\n"
    if anchor not in text:
        sys.exit("could not find updater.rs's test module")
    text = text.replace(anchor, anchor + TEST, 1)

open(path, "w", encoding="utf-8").write(text)
PY

grep -n 'pub const GAME_PROCESSES' "$src"

if ((test)); then
    (cd "$root" && cargo test -q -p pt-client updater:: 2>&1 | tail -n 4)
fi

if ((install)); then
    (cd "$root" && cargo build --release -q -p pt-client)
    install -m755 "$root/target/release/plasmatrack-client" "$HOME/.local/bin/plasmatrack-client"
    if systemctl --user is-active --quiet plasmatrack-client.service; then
        systemctl --user restart plasmatrack-client.service
        echo "installed and restarted plasmatrack-client.service"
    else
        echo "installed ~/.local/bin/plasmatrack-client"
    fi
fi
