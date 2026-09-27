# Installing the plugin

The PlasmaTrack client will automate these steps (stage 2). Until then, install by hand.

Put **only** these two files into the game's plugin folder:
- `plasmatrack.dll` (Windows and Proton) or `plasmatrack.so` (native Linux build);
- optionally, `plasmatrack.ini`.

Do **not** also register the DLL under
`HKLM\SOFTWARE\SCS Software\<game>\Plugins`. The SDK readme says a plugin listed twice gives
undefined behaviour.

The plugin creates `plasmatrack/` next to itself for its journal and captures, so that
folder must be writable.

## Windows (native)

```
<Steam library>\steamapps\common\Euro Truck Simulator 2\bin\win_x64\plugins\plasmatrack.dll
<Steam library>\steamapps\common\American Truck Simulator\bin\win_x64\plugins\plasmatrack.dll
```

Create the `plugins` folder if it does not exist. The game's log (`game.log.txt` in the game's
documents folder) shows `[plasmatrack] telemetry initialized, session …` when the plugin loads.

## Linux, game running under Proton (primary Linux case)

Use the **Windows DLL** in the same `bin/win_x64/plugins/` folder of the Linux Steam library:

```
~/.local/share/Steam/steamapps/common/<game>/bin/win_x64/plugins/plasmatrack.dll       # native Steam (default library)
~/.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common/<game>/bin/win_x64/plugins/plasmatrack.dll   # Flatpak Steam (default library)
```

The library can live elsewhere. In Steam, open Properties → Installed Files → Browse to find it.

**Networking.** The plugin connects to `127.0.0.1:<port>`, where the native Linux PlasmaTrack
client listens. According to the source code:
- Wine maps Winsock onto host sockets.
- The Steam Linux Runtime container (pressure-vessel) keeps the host network namespace.
- Flatpak Steam shares the host network, unless a user override removes it.

This has **not yet been confirmed on real hardware**, so please run the check below once on
each setup you use: native Steam and Flatpak Steam.

### Loopback check (Linux)

1. Keep the Flatpak network permission (Flatpak Steam only):
   ```sh
   flatpak info --show-permissions com.valvesoftware.Steam | grep -i shared   # expect: network
   flatpak override --show com.valvesoftware.Steam; flatpak override --user --show com.valvesoftware.Steam
   # neither override may contain "unshare=network" / "!network"
   ```
2. Listen as the client would, on the default port:
   ```sh
   nc -lk 127.0.0.1 30455 | head -c 400 | strings
   ```
3. Start the game with the plugin installed. Within a second of the game starting, `nc` should
   print JSON that includes `"kind":"hello"` and `"game_id":"eut2"` (or `"ats"`). If it does,
   loopback works across Proton (and Flatpak).
4. Optional namespace check while the game runs:
   ```sh
   readlink /proc/self/ns/net
   readlink /proc/$(pgrep -f 'eurotrucks2.exe|amtrucks.exe' | head -1)/ns/net   # must match the line above
   ```

## Native Linux build of the game (secondary)

```
<Steam library>/steamapps/common/<game>/bin/linux_x64/plugins/plasmatrack.so
```

## `plasmatrack.ini`

The client will write this file. All keys are optional.

```ini
port = 30455            ; must match [local].port of the client
sample_rate_hz = 20     ; samples per second of simulation time (0 = every frame)
replay_buffer_s = 120   ; seconds of samples kept for a reconnecting client
capture = 0             ; 1 = capture mode (docs/CAPTURE.md)
journal_max_mib = 64    ; cap for one session's on-disk journal
```
