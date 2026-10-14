# Capture protocol

The save-load detector, the refuel heuristic, profile and TruckersMP detection, and the event
ordering around deliveries all rest on game behaviour that the SDK headers do not document.
These captures turn that behaviour into facts and replay fixtures. Every value marked
PROVISIONAL in `config/plasmatrack.sample.toml` gets calibrated from them.

**Status (2026-09-28):**
- **Captured:** four ATS sessions on 2026-09-28, all on ATS 1.61.2.0s. One of them was on
  TruckersMP. Their findings are in `docs/PLAN.md` §3 and §6.
- **Regression tests:** four of them, trimmed, are regression tests in
  `crates/pt-replay/tests/fixtures/real/`.
- **Not captured yet:** nothing from ETS2.

## Setup

1. Install the plugin: run PlasmaTrack Setup (`installer/`), or follow `docs/INSTALL.md`. The
   client is not needed for capturing, because capture mode writes files on its own.
2. Next to the DLL, add this line to `plasmatrack.ini`:
   ```ini
   capture = 1
   ```
   In capture mode the plugin samples **every physics frame** and appends every message to
   `<plugins>/plasmatrack/captures/<session>.jsonl`. A new file is started on every game start
   or `sdk reload`.
3. For each scenario below, write down the wall-clock time and what you did, in order. A short
   `notes.txt` per game is enough.

## Scenarios (do each in ETS2 and in ATS)

Status marks: ✅ captured and understood · ◐ partly captured · — not captured yet · n/a does not
apply.

| # | Scenario | What to do | ATS | ETS2 |
|---|---|---|---|---|
| C1 | Quick-save → drive → quick-load | On a job: quick-save, drive ≥ 2 km, quick-load. | ✅ five manual-save loads (TruckersMP capture) | n/a: loads work the same in both games |
| C2 | Autosave load | On a job, load the latest autosave from the menu. | ✅ `autosave_drive_7`; also in the Sioux Falls session | — |
| C3 | Load from main menu | Exit to the main menu, then load a mid-job save. | ✅ raises `timer_restart`; in-game loads do not | — |
| C4 | Crash mid-job | On a job, kill the game process (Task Manager / `kill -9`). Restart it and continue from the autosave. | ✅ a real crash in the Sioux Falls session; the job continued and was scored | — |
| C5 | Load forward | Save A mid-job, drive on, save B. Load A, then load B. | ✅ slot 143 after 139: forward loads carry no rollback evidence | — |
| C6 | Load into a delivered job | Deliver a job, then load a save from the middle of that job. | — | — |
| C7 | Delivery | Deliver normally, and note when the results screen appears and closes. | ✅ Chicago → Chicago (3.6 points), Chicago → Sioux Falls (36.1 points) | — |
| C8 | Cancel | Cancel a job from the job info screen. | ✅ empty `job`, then `job.cancelled` | — |
| C9 | Ferry and train | Take one ferry and one train (or Channel Tunnel) crossing. | ✅ ferry (event, no odometer change); vanilla ATS has no trains, but mods can add them (you, 2026-10-13) and they are handled like any train: a teleport that adds no distance | — train still needed |
| C10 | Refuel | Refuel at a station: once partially, once full. | ✅ 0.3 s steps, about 54 L/s | — |
| C11 | Quick job accept | Take a quick job (company truck, teleport). | ◐ quick-job config seen after a profile switch; accepting one not recorded | — |
| C12 | Truck purchase / switch | Buy or switch trucks. | n/a: truck buying is separate from job logging (you, 2026-10-13) | n/a |
| C13 | Repair / service teleport | Use "call for help" or a service that teleports the truck. | ✅ F7: position jump inside a pause, no config change, no load signature | — |
| C14 | Profile switch | With a job active in profile P1, switch to profile P2 (also with a job), then back to P1. | ◐ one switch via the main menu; the back-and-forth with jobs in both profiles is still needed | — |
| C15 | Pause behaviour | ESC menu for ~10 s, map screen, photo mode. | ✅ ESC pauses (samples stop, heartbeats continue); map screen and photo mode are not tracked (you, 2026-10-13) | n/a |
| C16 | TruckersMP session | One short drive on TruckersMP. | ◐ `core_atsmp.dll` found; **redo with the new plugin** to check the TruckersMP SDK side | — |
| C17 | Convoy session | One short drive in Convoy (multiplayer time offset). | — | — |
| C18 | Falling under the map | Only if it happens naturally. Note it if it does. | n/a | n/a |

### Save edits (one change per recording, all mid-job)

For each: save on a job, exit to the main menu, edit that save, load it, and drive about 1 km.
Note the times and exactly what you changed. If saves aren't plain text, `uset g_save_format "2"`
in `config.cfg` should make the game write them that way (unverified).

| # | Edit | What it settles | ATS | ETS2 |
|---|---|---|---|---|
| C19 | Change the job's cargo, keeping the route | What a `cargo_switch` looks like, which is sent to review. | — | — |
| C20 | Change the truck | That a truck change mid-job is followed correctly, since it is allowed. | — | — |
| C21 | Raise the fuel | That fuel spikes across loads are ignored. | — | — |
| C22 | Change the truck odometer | That distance never depends on the absolute odometer value. | ✅ odometer ×10 across a load added no distance | — |
| — | Change the job's driven distance (not planned) | What a tampered `distance.km` looks like. | ✅ 1000 km reported at delivery; the cross-check sends it to review | — |

### Added after the first captures

| # | Scenario | What to do | ATS | ETS2 |
|---|---|---|---|---|
| C23 | Bobtail during a job | After the pickup, unhook the trailer, drive about 2 km without it, re-hook it and deliver. Settles whether `distance.km` includes bobtail driving (it includes the empty leg). | — | — |
| C24 | Delivery through a city | A clean delivery that passes through a city. Confirms ETS2 scales (19 on the road, 3 in cities) and the cross-check. | ✅ (C7) | — |
| C25 | TruckersMP with the SDK plugin | Start TruckersMP with the current plugin and join a server for a minute. Checks that the TruckersMP client loads `plasmatrack.dll` as its plugin, and what `server_id` and `account_id` look like. | — | — |

## What remains, in order of value

1. **ETS2:** C7/C24 (a clean delivery through a city) and C9 (train). Loads, cancel, refuel,
   F7 and crashes behave the same as in ATS and are not repeated.
2. **C25:** TruckersMP with the SDK plugin.
3. **C23:** bobtail during a job.
4. **C6, C11, C14:** load into a delivered job, accepting a quick job, and the full profile
   back-and-forth. These confirm that W (the load signature) does not appear where it should not.
5. **C19–C21:** the remaining save edits.
6. **C17:** Convoy.

## After capturing

1. Set `capture = 0` again. Capture files grow quickly, roughly 60 samples per second.
2. Send me the `captures/*.jsonl` files, your notes, and each game's `game.log.txt` together with
   the list of profile folders.

**Settled so far** (details in PLAN §3):
- which signals fire on each load path;
- the order of the job config and `job.delivered` / `job.cancelled`;
- ferry event timing relative to the position jump;
- odometer and fuel behaviour across loads;
- the refuel fill rate;
- that the job fingerprint is byte-identical across loads;
- the TruckersMP module name for ATS (`core_atsmp.dll`);
- that the active profile can be read from `game.log.txt`;
- that `distance.km` is in game scale and includes the empty leg.

**Still open:**
- train timing;
- a truck change through a save edit (C20);
- bobtail in `distance.km`;
- ETS2 behaviour;
- Convoy;
- the TruckersMP SDK in the real TruckersMP client.
