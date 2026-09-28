# PlasmaTrack Setup (Windows installer)

A small Electron app that puts PlasmaTrack on a player's PC. It is a thin UI over
`plasmatrack-client` commands (docs/CLIENT.md). Everything that touches game folders
or the config is done by the client, where it is tested.

## What Install does

1. Stops a running background client with `plasmatrack-client stop`, so its tray icon goes away
   cleanly. If it is still running after 5 s, it is killed.
2. Copies the payload to `%LOCALAPPDATA%\PlasmaTrack\bin`: the client, the plugin DLL,
   licence texts and `manifest.json`.
3. `plasmatrack-client set-local`: saves the server address and player token into
   `[local]` of `%APPDATA%\PlasmaTrack\plasmatrack.toml`, and creates the file from the
   sample config if it is missing. The token goes over stdin, never on a command line.
   An empty token field keeps the saved one.
4. `plasmatrack-client install-plugin`: copies `plasmatrack.dll` into
   `bin\win_x64\plugins\` of every ETS2/ATS install in every Steam library, then writes
   `plasmatrack.ini` there. Existing lines such as `capture = 1` are kept; `port`,
   `sample_rate_hz` and `replay_buffer_s` come from the config.
5. Adds a **PlasmaTrack** Start menu shortcut to `plasmatrack-clientw.exe run`.
6. Optionally adds `"…\plasmatrack-clientw.exe" run` to
   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (value `PlasmaTrack`). This is on by
   default.
7. Starts the client and waits for its status page.

**Running in the background.** `plasmatrack-clientw` has no window. It shows a purple **P** in the
taskbar tray:
- **Tooltip:** whether the game is connected and whether uploads are working.
- **Double-click:** opens the status page.
- **Right-click menu:** Open status page, Open data folder, Quit PlasmaTrack.
- **Starting it again** (Start menu) while it runs just opens the status page.
- **Startup errors** (for example a missing config) appear as a dialog.

**Update** does the same again. **Uninstall** stops the client and removes the startup
entry, the Start menu shortcut, the plugin, `plasmatrack.ini` and the `bin` folder. Local data, settings and the
token are kept unless the player ticks the box. The plugin's `plasmatrack\` journal
folder in each game is left in place.

No administrator rights are needed when Steam is in its default, user-writable folder.
The game must be closed so the DLL can be replaced.

## Building

Needs Node.js LTS (portable zip is fine) and the release binaries:

```powershell
cargo build --release -p pt-client
powershell -ExecutionPolicy Bypass -File scripts/build-plugin.ps1 -Config Release
cd installer
npm ci
npm run dist        # dist\PlasmaTrack-Setup-<version>.exe (portable, ~100 MB)
```

`npm start` runs it unpackaged. `"PlasmaTrack Setup.exe" --install [--no-autostart]` or
`--uninstall` runs without a window and prints the steps as JSON. It keeps the saved server and
token; a token is never accepted on the command line. `npx electron scripts/snapshot.js out.png [dark]` saves a
PNG of the window and quits; it only reads state.

**Inside VS Code** the terminal inherits `ELECTRON_RUN_AS_NODE=1`, which makes Electron run
as plain Node. Clear it first (`Remove-Item Env:ELECTRON_RUN_AS_NODE`).

The exe is not code-signed, so Windows SmartScreen warns on first run.

## Security

- The page runs sandboxed, with context isolation and without Node.
- The CSP allows only its own script and style.
- Navigation and new windows are blocked.
- The preload script exposes five calls. The only external page it can open is the local
  status page on `127.0.0.1`.
