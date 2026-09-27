# Capture protocol

The save-load detector, the refuel heuristic, profile and TruckersMP detection, and the event
ordering around deliveries all rest on game behaviour that the SDK headers do not document.
These captures turn that behaviour into facts and replay fixtures. Every value marked
PROVISIONAL in `config/plasmatrack.sample.toml` gets calibrated from them.

## Setup

1. Install the plugin as described in `docs/INSTALL.md`. The client is not needed: capture
   mode writes files on its own.
2. Next to the DLL, create `plasmatrack.ini` containing:
   ```ini
   capture = 1
   ```
   In capture mode the plugin samples **every physics frame** and appends every message to
   `<plugins>/plasmatrack/captures/<session>.jsonl`. A new file is started on every game start
   or `sdk reload`.
3. For each scenario below, write down the wall-clock time and what you did, in order. A short
   `notes.txt` per game is enough.

## Scenarios (do each in ETS2 and in ATS)

| # | Scenario | What to do |
|---|---|---|
| C1 | Quick-save → drive → quick-load | On a job: quick-save, drive ≥ 2 km, quick-load. |
| C2 | Autosave load | On a job, load the latest autosave from the menu. |
| C3 | Load from main menu | Exit to the main menu, then load a mid-job save. |
| C4 | Crash mid-job | On a job, kill the game process (Task Manager / `kill -9`). Restart it and continue from the autosave. |
| C5 | Load forward | Save A mid-job, drive on, save B. Load A, then load B. |
| C6 | Load into a delivered job | Deliver a job, then load a save from the middle of that job. |
| C7 | Delivery | Deliver normally, and note when the results screen appears and closes. |
| C8 | Cancel | Cancel a job from the job info screen. |
| C9 | Ferry and train | Take one ferry and one train (or Channel Tunnel) crossing. |
| C10 | Refuel | Refuel at a station: once partially, once full. |
| C11 | Quick job accept | Take a quick job (company truck, teleport). |
| C12 | Truck purchase / switch | Buy or switch trucks. |
| C13 | Repair / service teleport | Use "call for help" or a service that teleports the truck. |
| C14 | Profile switch | With a job active in profile P1, switch to profile P2 (also with a job), then back to P1. |
| C15 | Pause behaviour | ESC menu for ~10 s, map screen, photo mode. |
| C16 | TruckersMP session | One short drive on TruckersMP (for the loaded module names). |
| C17 | Convoy session | One short drive in Convoy (multiplayer time offset). |
| C18 | Falling under the map | Only if it happens naturally. Note it if it does. |

## After capturing

1. Set `capture = 0` again. Capture files grow quickly, roughly 60 samples per second.
2. Send me the `captures/*.jsonl` files, your notes, and each game's `game.log.txt` together with
   the list of profile folders. Profile detection may come from those files.

What the captures will settle:
- which signals fire on each load path;
- the order of the job config and `job.delivered` / `job.cancelled`;
- the timing of ferry and train events relative to the position jump;
- odometer and fuel behaviour across loads and truck changes;
- the refuel fill rate;
- whether the job fingerprint is byte-identical across loads;
- the TruckersMP module names;
- where the active profile can be read.
