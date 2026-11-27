PlasmaTrack for Linux (ETS2 / ATS under Proton)

Install:    ./install.sh      (needs your token from /token new on Discord)
Upgrade:    download the new package and run its ./install.sh
Remove:     ~/.local/share/plasmatrack/bin/uninstall.sh  [--delete-data]

Run the plasmatrack-client in this package. A client you build yourself has a
different fingerprint, and the server discards its jobs.

There is no automatic update on Linux: ask the Discord bot (/download) for the
newest package.

Logs:       journalctl --user -u plasmatrack-client
Status:     http://127.0.0.1:30456/
