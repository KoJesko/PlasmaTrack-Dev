# Capture protocol

The save-load detector, the refuel heuristic, profile and TruckersMP detection, and the event
ordering around deliveries all rest on game behaviour that the SDK headers do not document.
These captures turn that behaviour into facts and replay fixtures. Every value marked
PROVISIONAL in `config/plasmatrack.sample.toml` gets calibrated from them.

**Status (2026-10-13):**
- **Captured:** four ATS sessions (2026-09-28, ATS 1.61.2.0s, one of them on TruckersMP) and two
  ETS2 sessions (2026-10-13, ETS2 1.61.1.1s: one on TruckersMP, one in Convoy).
- **Regression tests:** all six, trimmed, are regression tests in
  `crates/pt-replay/tests/fixtures/real/`.
- **Findings:** in `docs/PLAN.md` §3 and §6.

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

## Scenarios

ETS2 and ATS share an engine, so scenarios captured in one game are not repeated in the other
unless the games differ (scales, trains, TruckersMP module names).

Status marks: ✅ captured and understood · ◐ partly captured · — not captured yet · n/a does not
apply or not needed.

| # | Scenario | What to do | ATS | ETS2 |
|---|---|---|---|---|
| C1 | Quick-save → drive → quick-load | On a job: quick-save, drive ≥ 2 km, quick-load. | ✅ five manual-save loads (TruckersMP capture) | n/a: same as ATS |
| C2 | Autosave load | On a job, load the latest autosave from the menu. | ✅ `autosave_drive_7`; also in the Sioux Falls session | ✅ autosave loads on each profile switch |
| C3 | Load from main menu | Exit to the main menu, then load a mid-job save. | ✅ raises `timer_restart`; in-game loads do not | ✅ same: `timer_restart` on each switch |
| C4 | Crash mid-job | On a job, kill the game process (Task Manager / `kill -9`). Restart it and continue from the autosave. | ✅ a real crash in the Sioux Falls session; the job continued and was scored | n/a: same as ATS |
| C5 | Load forward | Save A mid-job, drive on, save B. Load A, then load B. | ✅ slot 143 after 139: forward loads carry no rollback evidence | n/a: same as ATS |
| C6 | Load into a delivered job | Deliver a job, then load a save from the middle of that job. | — | ✅ a manual save loaded 7 s after delivery, then delivered again; counted once |
| C7 | Delivery | Deliver normally, and note when the results screen appears and closes. | ✅ Chicago → Chicago (3.6 points), Chicago → Sioux Falls (36.1 points) | ✅ Dover → Calais (7.0 points), a 1 km quick job (0.2) |
| C8 | Cancel | Cancel a job from the job info screen. | ✅ empty `job`, then `job.cancelled` | n/a: same as ATS |
| C9 | Ferry and train | Take one ferry and one train (or Channel Tunnel) crossing. | ✅ ferry (event, no odometer change); vanilla ATS has no trains, but mods can add them (you, 2026-10-13) and they are handled like any train: a teleport that adds no distance | ✅ five ferries and the Channel Tunnel: event inside a pause, 6.57 world km jump, odometer unchanged, not in `distance.km` |
| C10 | Refuel | Refuel at a station: once partially, once full. | ✅ 0.3 s steps, about 54 L/s | n/a: same as ATS |
| C11 | Quick job accept | Take a quick job (company truck, teleport). | ◐ quick-job config seen after a profile switch | ✅ two accepted: `[job selection]` in the log, truck and trailer configs change |
| C12 | Truck purchase / switch | Buy or switch trucks. | n/a: truck buying is separate from job logging (you, 2026-10-13) | n/a |
| C13 | Repair / service teleport | Use "call for help" or a service that teleports the truck. | ✅ F7: position jump inside a pause, no config change, no load signature | n/a: same as ATS |
| C14 | Profile switch | With a job active in profile P1, switch to profile P2 (also with a job), then back to P1. | ◐ one switch via the main menu | ✅ A (quick job) → B (quick job) → A, through TruckersMP server changes |
| C15 | Pause behaviour | ESC menu for ~10 s, map screen, photo mode. | ✅ ESC pauses (samples stop, heartbeats continue); map screen and photo mode are not tracked (you, 2026-10-13) | n/a |
| C16 | TruckersMP session | One short drive on TruckersMP. | ✅ `core_atsmp.dll` | ✅ `core_ets2mp.dll` |
| C17 | Convoy session | One short drive in Convoy (multiplayer time offset). | n/a: same as ETS2 | ✅ hosted alone: offset 3–5 in the session, TruckersMP not loaded, hosting is not a load; job labelled Convoy |
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
| C22 | Change the truck odometer | That distance never depends on the absolute odometer value. | ✅ odometer ×10 across a load added no distance | n/a |
| — | Change the job's driven distance (not planned) | What a tampered `distance.km` looks like. | ✅ 1000 km reported at delivery; the cross-check sends it to review | n/a |

### Added after the first captures

| # | Scenario | What to do | ATS | ETS2 |
|---|---|---|---|---|
| C23 | Bobtail during a job | After the pickup, unhook the trailer, drive about 2 km without it, re-hook it and deliver. Settles whether `distance.km` includes bobtail driving (it includes the empty leg). | — | ✅ Dover → Calais cargo market: 113 game km to the pickup with the trailer hooked and no cargo (the only way to bobtail without World of Trucks, you, 2026-10-13). `distance.km` includes it (239 vs 238.87 tracked); it earns no points |
| C24 | Delivery through a city | A clean delivery that passes through a city. Confirms the scales and the cross-check. | ✅ (C7) | ✅ city 3, road 19, **UK roads 15**; 239 reported vs 238.87 tracked |
| C25 | TruckersMP with the SDK plugin | Start TruckersMP with the current plugin and join a server for a minute. | n/a: ETS2 settled it | ✅ loaded by the TruckersMP client; connected, server IDs 4 and 30, account ID (briefly 0 while reconnecting) |

## What remains, in order of value

1. **C19–C21:** the cargo, truck and fuel save edits (either game).
2. Optional: joining someone else's Convoy, and leaving one mid-job and driving on in singleplayer.

## After capturing

1. Set `capture = 0` again. Capture files grow quickly, roughly 60 samples per second.
2. Send me the `captures/*.jsonl` files, your notes, and each game's `game.log.txt` together with
   the list of profile folders.

**Settled so far** (details in PLAN §3):
- which signals fire on each load path, including loading into a delivered job;
- the order of the job config and `job.delivered` / `job.cancelled`;
- ferry and train event timing relative to the position jump;
- odometer and fuel behaviour across loads;
- the refuel fill rate;
- that the job fingerprint is byte-identical across loads;
- the TruckersMP module names (`core_atsmp.dll`, `core_ets2mp.dll`) and the TruckersMP SDK in
  the real TruckersMP client;
- that the active profile can be read from `game.log.txt`, and how profile switches look;
- that `distance.km` is in game scale and in whole km, includes the empty leg and bobtail
  driving and leaves out
  ferries and trains;
- the scales of both games (ETS2 UK roads are 15).
- Convoy detection, and that hosting a Convoy is not a load.

**Still open:**
- a cargo or truck change through a save edit (C19, C20);
- joining someone else's Convoy, and leaving one mid-job.
