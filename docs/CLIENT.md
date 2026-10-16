# PlasmaTrack client

`plasmatrack-client` runs in the background on the player's PC (Windows or Linux).
- It receives the plugin's telemetry on `127.0.0.1`.
- It keeps everything in a local SQLite log, so it works offline.
- It uploads the raw data to the PlasmaTrack server.

It does not score anything itself; the server is authoritative. Provisional results and
notifications arrive with the validation module (stage 3).

## First run

On Windows, players run **PlasmaTrack Setup** (`installer/`). It does all of this: it installs
the plugin, saves the server and token, adds the client to startup and starts it. By hand:

```sh
plasmatrack-client init-config    # writes the sample config (path printed)
# edit [local] in that file: server_url, token (issued by the server via the Discord bot)
plasmatrack-client run
```

**Default locations**

| | Windows | Linux |
|---|---|---|
| config | `%APPDATA%\PlasmaTrack\plasmatrack.toml` | `~/.config/plasmatrack/plasmatrack.toml` |
| local log | `%LOCALAPPDATA%\PlasmaTrack\client.sqlite` | `~/.local/share/plasmatrack/client.sqlite` |

Both can be overridden with `--config PATH` and `--data-dir PATH`.

**Only `[local]` may be edited per driver.** Every other key must match a config version the
server published. Otherwise the server discards the job (rule 0, PLAN §6a).

## What `run` does

1. **Finds ETS2/ATS** in every Steam library: native, Flatpak and Snap Steam on Linux, and
   `libraryfolders.vdf` on Windows.
2. **Writes `plasmatrack.ini`** next to every installed plugin, setting `port`,
   `sample_rate_hz` and `replay_buffer_s`. Other lines, such as `capture = 1`, are kept.
3. **Listens** on `[local] listen_address:port` (default `127.0.0.1:30455`) for the plugin.
   - Every frame is committed to SQLite before it is acknowledged, so the plugin never
     drops anything the client has not stored.
   - Replays are de-duplicated by (session, seq).
4. **Uploads** unsent frames to `[local] server_url` in chunks (`docs/UPLOAD_API.md`).
   - Frames are sent in batches about every 10 s (0.2.2; before, every few frames). A full chunk
     and a game event (a delivery, a cancellation) go at once.
   - A failed upload is retried every `[sync] retry_interval_wall_s` (20 s), forever.
   - Unsent data survives restarts.
   - The full list of DLLs in the game process never leaves the PC; only TruckersMP matches
     are uploaded.
   - Each upload carries the config hash and the install manifest (SHA-256 of the client, the
     config and every installed plugin).
5. **Follows the game logs** (`Documents/<game>/game.log.txt`) to tell profiles apart. Each
   time a save loads, it records which profile it belongs to: a hash of the profile folder,
   never the profile name.
6. **Runs the job rules live** on what it stores, the same code the server runs. Results are
   provisional; the server's are the ones that count.
   - **Alerts:** a desktop notification when a delivery is Invalid, or when the speed penalty
     activates. Each comes 5–30 s late on purpose (`[notifications]`). Windows focus assist
     can hold them while a game runs fullscreen.
   - After a restart the client rebuilds this from the sessions of the last
     `[timeline] reopen_window_wall_h` (48 h), without alerting again.
7. **Serves the status page** at `http://127.0.0.1:<status_port>/`, with JSON at
   `/status.json`. The page lists the jobs (provisional), with
   JSON at `/jobs.json`.

## Other commands

```sh
plasmatrack-client status            # counts from the local log (works while the client is stopped)
plasmatrack-client find-games [--json]   # where ETS2/ATS were found and whether the plugin is installed
plasmatrack-client write-ini         # (re)write plasmatrack.ini next to installed plugins
plasmatrack-client install-plugin --plugin PATH   # copy the plugin into every game found, write the ini
plasmatrack-client uninstall-plugin  # remove the plugin and plasmatrack.ini again
plasmatrack-client set-local         # stdin: {"server_url": "...", "token": "..."} (both optional)
plasmatrack-client show-local        # [local] as JSON; the token only as token_set
plasmatrack-client check-update      # download and verify a newer release, without installing it
plasmatrack-client uninstall [--delete-data] [--json]   # remove PlasmaTrack (as PlasmaTrack-Uninstall.exe does)
plasmatrack-client register          # add PlasmaTrack to Windows' Installed apps (Setup and updates do this)
```

**Updates** (docs/UPDATES.md). The windowless client installed by Setup checks the server every
6 hours and downloads newer releases. It installs them once ETS2 and ATS are closed, with
`apply-update` (not meant to be run by hand). The status page shows any pending update. Updates need the player token.

**`plasmatrack-uninstall`** (published as `PlasmaTrack-Uninstall.exe`) removes PlasmaTrack from
the PC. It asks first, and asks again before deleting local data and the token. `--quiet` asks
nothing. Run from `bin`, it continues from a copy in the temp folder, so it can delete `bin`.

- `set-local` edits only `[local]` and keeps comments. It creates the config from the sample
  if it is missing. The token is read from stdin so it never appears in a process list.
- `install-plugin` and `uninstall-plugin` print one JSON result per game folder. They fail
  when any folder fails (usually because the game is running).

**`plasmatrack-clientw`** is the same program without a console window on Windows. The
installer starts `plasmatrack-clientw run` at login.
- **Log:** `client.log` in the data directory. The previous log is kept as `client.log.old`
  once it passes 8 MiB.
- **Tray icon:** a purple P. The tooltip shows the game connection and the upload state.
  Double-clicking opens the status page. The menu has Open status page, Open data folder and
  Quit.
- **Startup errors** appear as a dialog.

**Only one client runs at a time.**
- A second `run` finds the first through its status page. The windowless one opens that page;
  the console one prints its address and exits.
- `plasmatrack-client stop` asks the running client to quit. It sends `POST /quit` with the
  header `x-plasmatrack: quit`; requests carrying an `Origin` header are refused, so web pages
  cannot do it.
- `taskkill` without `/F` also stops `plasmatrack-clientw` cleanly.
