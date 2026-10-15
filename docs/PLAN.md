# PlasmaTrack — Plan (awaiting approval)

Status: foundation committed (repo layout, vendored SDK 1.14, `pt-core` skeleton, sample config; `cargo test` green).
Nothing below is built yet. Header citations are relative to `third_party/scssdk-1.14/include/`.

Requirement changes folded in from our conversation:
- No overlay of any kind. The local OBS page, desktop window and live portal overlay are all dropped.
- The public map shows each job's route on its own page.
- ATS and ETS2 are separate everywhere: jobs, totals, leaderboards, maps and timeline state.
- Minimum game patch is 1.36. Older versions are unsupported, and those players submit manually.
- Failed uploads retry every 20 s, forever.
- The full save-load rules (§5).

---

## 1. Stack

| Part | Choice | Why |
|---|---|---|
| Plugin | C++17, CMake + Ninja. Primary toolchain is **llvm-mingw (UCRT)**; distro `x86_64-w64-mingw32-g++-posix` is the fallback. Two toolchain files (llvm-mingw layout, Debian layout). Fully static (`-static`), exports via `.def` | The SDK headers are C++-only (they do not compile as C). One toolchain covers Windows and Linux-cross, needs no thread-model choice, and is FOSS (ISC / Apache-2.0-LLVM / ZPL). A static build keeps the game directory free of runtime DLLs. |
| Client | **Rust**, headless background app with a small status/settings page on `127.0.0.1` | The overlay is gone, so a webview is unnecessary. This avoids Tauri's proprietary MSVC Build Tools and WebView2. The GNU Rust toolchain is already installed. The client runs the exact same `pt-core` code as the server. |
| Server | **Rust on Cloudflare Workers** (Workers Paid; decision 15). The `worker` crate (workers-rs, Apache-2.0) compiled to wasm, with D1 (SQLite) for records, R2 for raw chunks, job points and timeline snapshots, one Durable Object per (player, game) that runs the timeline, Queues for webhooks and bulk recalculation, and a Cron Trigger for revocations (§7a) | The same `pt-core` crate runs in wasm. All request logic lives in `pt-server`, plain Rust tested natively against rusqlite (D1 is SQLite); `pt-worker` only binds it to Cloudflare. |
| Shared | `pt-core`: pure, with no I/O, no clock and no RNG | Deterministic. Client and server produce bit-identical results. The replay harness uses it directly. |
| Web | **None.** There is no website. All public info (per-job route maps, leaderboards) and all admin actions go through the **Discord bot (separate repo)**, which consumes this server's JSON API | Your decision. This repo owns the API contract and route projection. |
| Admin | Admin API + signed webhooks consumed by your **Discord bot (separate repo)** | Your decision; this repo only owns the API contract. |
| Auth | The bot: two service tokens plus the acting moderator's Discord ID (API_CONVENTIONS §2). Players: 256-bit server-issued token, stored as SHA-256 | As specified. There is no admin login: moderators act through the bot. |

Rejected alternatives:
- **Tauri:** needs MSVC Build Tools (proprietary). Its only purpose was the overlay, which is now dropped.
- **Go/Wails:** no advantage here.
- **Mapbox GL v2+:** proprietary.

Every dependency is FOSS. `THIRD_PARTY_NOTICES` will carry the SCS SDK MIT notice, the mingw-w64 runtime `COPYING` file, and the winpthreads license.

## 2. Repo layout

```
Cargo.toml                 workspace
third_party/scssdk-1.14/   SDK headers, readme, MIT license, PROVENANCE.md
plugin/                    CMakeLists.txt, cmake/toolchain-{llvm-mingw,debian-mingw}.cmake,
                           src/ (plugin.cpp, spsc_ring.hpp, sender.cpp, journal.cpp, wire.cpp, net_{win,posix}.cpp),
                           plasmatrack.def, tests/ (fake-game harness driving scs_telemetry_init with mock registrars)
crates/pt-core/            units, config, wire decode, timeline reconstruction, rules, route projection
crates/pt-replay/          CLI: replay a trace file through pt-core, diff against expected results
crates/pt-client/          binary: plugin listener, SQLite log, uploader, status page
crates/pt-server/          library: ingest, scoring, public/admin API, webhooks (native-testable)
crates/pt-worker/          Cloudflare Worker: binds pt-server to D1, R2, Durable Objects, Queues
fixtures/                  *.trace.jsonl + *.expected.json (one per rule / boundary / load scenario)
config/                    plasmatrack.sample.toml
scripts/                   install (Windows / Proton / Flatpak), loopback verification, capture tooling
docs/                      this plan, capture protocol, install guide
```

## 3. SDK channels, configs and events used (all verified in the headers)

**Init and game identification**
- Export `scs_telemetry_init` / `scs_telemetry_shutdown` (scssdk_telemetry.h:93,101).
- Accept only `SCS_TELEMETRY_VERSION_1_01`. Gameplay events and s64 values need it (scssdk_telemetry.h:23,27).
- `game_id` is `"eut2"` (eurotrucks2/scssdk_eut2.h:16) or `"ats"` (amtrucks/scssdk_ats.h:16), read from `common.game_id` (scssdk.h:169).
- Minimum telemetry game version (both correspond to patch 1.36):
  - ETS2 `1.15`: eurotrucks2/scssdk_telemetry_eut2.h:53,74
  - ATS `1.02`: amtrucks/scssdk_telemetry_ats.h:32,40
- ATS's header does not include the gameplay-events header, so the plugin includes `common/scssdk_telemetry_common_gameplay_events.h` itself.

**Events** (scssdk_telemetry_event.h)

| Event | Lines | Notes |
|---|---|---|
| `configuration` (5) | :71 | Delivered after init, then only "if the configuration changed" (:84) |
| `gameplay` (6) | :78 | |
| `paused` (3) / `started` (4) | :49 / :54 | No channel data arrives while paused (:45-47) |
| `frame_start` | | Carries `paused_simulation_time` in µs (:136; unit per scssdk.h:79-81) and the `timer_restart` flag (:97) |
| `frame_end` | | |

Per-frame order (:82-89): configuration → paused/started → frame_start → gameplay → channels → frame_end.

**Configs** (common/scssdk_telemetry_common_configs.h)
- `job` (:122-146). An empty attribute set means there is no job (:125). Attributes used:
  - cargo: `cargo.id`, `cargo`, `cargo.mass` (kg), `cargo.unit.mass`, `cargo.unit.count`
  - source/destination: `source.city.id`, `source.city`, `source.company.id`, `source.company`, and the same four for `destination.*`. The company fields are absent for special-transport jobs.
  - `income`: u64, game currency
  - `delivery.time`: u32, absolute in-game minute when the delivery window ends
  - `planned_distance.km`: u32, simulated km, excludes ferries
  - `cargo.loaded`, `job.market`, `is.special.job`
- `truck` (:59-91): `brand_id`, `id`, `license.plate`, `license.plate.country.id`, `fuel.capacity` (litres).
- `trailer.N`: forwarded only for evidence. The un-indexed `trailer` alias duplicates `trailer.0` (:94) and is ignored.

**Gameplay events** (common/scssdk_telemetry_common_gameplay_events.h)
- `job.delivered` (:33):
  - `revenue` s64
  - `earned.xp` s32
  - `cargo.damage` float 0–1
  - `distance.km` float, "real distance in km on the job"
  - `delivery.time` u32, minutes spent on the job
  - `auto.park.used`, `auto.load.used`
- `job.cancelled` (:19): `cancel.penalty` s64
- `player.use.ferry` / `player.use.train` (:62/:74): `pay.amount`, `source.id`, `target.id`, `source.name`, `target.name`
- `player.fined`, `player.tollgate.paid`: forwarded for evidence only.

**Channels**

| Channel | Type / unit | Source | Use |
|---|---|---|---|
| `truck.speed` | float m/s, negative in reverse | truck_common_channels.h:107-114 | speed rules (the rules use its magnitude) |
| `truck.odometer` | float km | :640-645 | driven distance, rollback detection |
| `truck.fuel.amount` | float litres | :321-326 | fuel rules |
| `truck.world.placement` | dplacement; world X east, Y up, Z south | :33-38; scssdk_value.h:86-87 | route, discontinuities |
| `truck.local.velocity.linear` | fvector m/s | :41-46 | cross-check of speedometer speed (evidence only) |
| `game.time` | u32 in-game minutes | common_channels.h:277-285 | supporting signal in singleplayer only; ignored on TruckersMP (server-side time) |
| `local.scale` | float | :265-275 | scaling of odometer vs. position |
| `multiplayer.time.offset` | s32 minutes | :287-300 | multiplayer detection |
| `job.cargo.damage` | float 0–1 | job_common_channels.h:241-246 | evidence |

**Not in the SDK** (so PlasmaTrack derives or omits them):
- There is **no refuel event**. Only six gameplay events exist (gameplay_events.h:13-74).
- There is **no save-load event** and **no profile or save identifier**.
- There is **no job ID or job-start event**, and `job.delivered` / `job.cancelled` carry no job identity.
- There is **no on-ferry state**. Ferries and trains are instantaneous events.
- There is **no money or fuel-cost data**.
- The unit of `world.placement` is not stated. Community tooling treats it as metres; capture will confirm.

**MinGW pitfalls (found by the research)**
- The SDK's `SCS_PF_U64` expands to `"lu"`, which is wrong on Win64. Use `PRIu64` instead.
- The SDK example log calls have this bug too. Do not copy them.

**Observed in the real game** (ATS 1.61.2.0s, 2026-09-28, first plugin run; no capture mode yet):
- The telemetry game version reported is **1.07**, newer than the newest version the SDK 1.14 headers document (ATS 1.05).
- The game sends two **undocumented config ids, `bus_job` and `car_job`**. Both were empty during this run. The plugin forwards them raw, and their contents still need to be captured.
- **Every config is re-sent when a save loads into the world** (job, bus_job, car_job, controls, hshifter, truck, trailer, trailer.0–9). On the *first* world load after launch a **`timer_restart` flag** follows in the first frame; later loads do not raise it (see the capture below).
- At `scs_telemetry_init` the configs arrive once, empty, before any frame. They are sent again when the world loads.
- The plugin runs alongside TruckyTelemetry, an ETS shared-memory plugin and others without conflict.
- **Privacy note for stage 2:** the `modules` message lists every loaded DLL (155 here). The client keeps that list local and uploads only the matches against `[multiplayer] truckersmp_module_names`, never the full list.

**Second capture** (ATS 1.61.2.0s, 2026-09-28 10:13–10:40, sessions `01719a0c…` and `0ae3404d…`, plugin with `scale` messages):

Tampered delivery:
- **`job.delivered` `distance.km` is not trustworthy on its own.** A 7 km (planned) San Francisco job was delivered with **`distance.km = 1000`**, `delivery.time = 60`, `auto.park.used = true` and `auto.load.used = true` (gameplay_events.h:118-132).
  - Just before the delivery: a save load (slot 143) whose truck odometer was edited ×10 (12,888 → 128,880 km), then a 1.8 world-km teleport straight to the destination while driving. The game log shows free-camera "Camera speed" lines just before it, which suggests a developer-mode teleport.
  - **Confirmed (you, 2026-09-28): the 1000 km was a save edit of the job's driven distance.** The game keeps that figure in the save and reports it unchanged at delivery.
  - TruckyTelemetry reported the same `realDistance: 1000`.
  - The tracker's own loaded distance after the load was about 0.2 game km, so the delivered-vs-trace cross-check (§6) sends this job to review. That check is essential, not optional.
  - This delivery **cannot** answer whether `distance.km` is 1:1 or game scale. A clean delivery (no edits, no teleports) is still needed.

Save edits and profiles:
- **Odometer save edit across a load** adds no distance: distance is measured within segments, and loads split segments.
- **Profile switch via the main menu:**
  - `exit` → `New profile selected: '<name>'` in the game log → W (job, truck and trailer emptied), **plus a `timer_restart` and a reset of `paused_simulation_time`**;
  - the other profile's odometer, fuel, game time and position (a quick-job company truck: odometer 0.085 km, game.time 600).
  
  Loads from the main menu raise `timer_restart`; in-game loads did not (first capture).

Other events:
- **Refuel** (two stops, 624 → 840 L and 840 → 1041 L = full):
  - Fuel rises in steps every 18 frames (0.3 s of simulation): first about 0.62 L per step for about 3 s, then 16.18 L per step (about 54 L/s), up to capacity.
  - The truck was also moved short distances (31 m and 322 world m) around the pump.
  - These are the first real figures for the refuel heuristic.
- **Scale flapping:** `local.scale` switched 3 ↔ 20 six times in 0.8 s at a city boundary by the pump. This is harmless for distance, because each run is measured on its own. The route map should merge tiny runs for display.
- **Cargo loading** again skipped 48 game minutes, and it moved the truck 7.5 world m.
- **Quick job config:** `job.market = "quick_job"`, with a company truck (`vehicle.freightliner.cascadia2024`) and a flatbed trailer.

**First capture** (ATS 1.61.2.0s on TruckersMP, 2026-09-28 08:50–09:21, capture mode, 70,453 samples; the game hung on exit to the main menu, not in the plugin).

Loads:
- **Six in-session loads**: five manual saves (slots 139 and 143, back and forth) and one autosave (`autosave_drive_7`). All six had the same signature:
  1. `paused`.
  2. **`job`, `truck`, `trailer` and `trailer.0`–`9` all re-sent empty in one batch** at the moment the game log prints `Loading save`.
  3. 3–18 s later the full configs are re-sent.
  4. `started`.
  5. The first sample jumps in position, odometer and fuel. `game.time` jumps too, backwards or forwards.
- **No `timer_restart` on any in-session load.** `paused_simulation_time` and `simulation_time` keep increasing across loads. So `timer_restart` and a time reset cannot be relied on as load signals.
- **Forward loads carry no rollback evidence.** For example, loading slot 143 after slot 139 moved the odometer +4.548 km and the position 201 world m, with zero simulated time between them. The R + C rule (§5) could not classify that as a load; this is fixed in §5.

Job details:
- **Byte-identical across loads:** the job config was identical after all six loads.
- **`cargo.loaded` changes:** with an owned trailer on the cargo market, the job config arrives at accept with `cargo.loaded=false`. At the pickup, loading the cargo re-sends the job, truck and trailer configs (not emptied first), sets `cargo.loaded=true`, and skips 48 game minutes. It is correctly excluded from the fingerprint.

Other events:
- **Cancel:** `job` is re-sent empty, `truck` and `trailer` are re-sent full, and then `job.cancelled` arrives (with `cancel.penalty`), all in one pause.
- **Ferry:** the order is `paused` → `player.use.ferry` (with source/target ids and names and `pay.amount`) → `started`. The next sample jumps 1.3 world km in position (about 26 game km at scale 20) and 36 game minutes. **The odometer does not change** on a ferry.
- **Two teleports with no event and no config change**:
  - 1.76 world km with 118 game minutes skipped;
  - 6.47 world km with no game time skipped.

  **TruckersMP runs its own server-side clock (you, 2026-09-28).** A sleep or a ferry sometimes skips game time and sometimes does not. So on TruckersMP, `game.time` is neither evidence of a load nor evidence against a transport, and a missing time skip is never suspicious.
  
  Both happened inside a pause, and the odometer was unchanged. What they were:
- **The 1.76 world km / 118-minute jump was F7 (call for help).** Its order was: ferry at 25:40 (with its event) → autosave load at 26:17, back to before the ferry (odometer −35.8 game km) → F7 at 26:47 → the game saved right after.
- **The 6.47 world km jump** (27:44, after the cancel, while browsing the Olympia job; no game time skipped) is still unidentified.

Every ferry in this capture raised its event. Nothing depends on transport events anyway: a teleport is never a load, never adds distance, and is never suspicious by itself. Under the old rule, a position jump (R) plus a pause bracket (C) would have been **misclassified as a load**. This is fixed in §5.
- **Units (measured over 70k samples):** `truck.world.placement` is in world metres. `truck.odometer` advances at world distance × `local.scale`: the ratio was 2.98 at scale 3 (cities) and 20.00 at scale 20 (open road).
  - The odometer is therefore in the game's own kilometres, the same scale as job distances and the map. Points are scored on the **world (1:1) distance**, which is the odometer ÷ `local.scale` (§6, decision 22). The game distance is reported next to it.
  - `truck.speed` is world m/s, which is what the speedometer shows. Speed rules 2 and 5 use it.
  - Position thresholds in the config are named `_world_m`. Never compare position distances with odometer distances without multiplying by `local.scale`.
- **Samples stop while paused.** Frames continue, and heartbeats keep coming.

Profile and TruckersMP:
- **Profile:** the telemetry does not carry it, but `Documents/<game>/game.log.txt` does:
  - `New profile selected: '<name>'`;
  - every `Loading save. Type: <t>, slot: <n>, path: .../profiles/<hex of profile name>/save/<slot>/game.sii`.
  
  Observed types: 0 = autosave on start, 3 = drive autosave, 5 = manual save.
- **TruckersMP:** `core_atsmp.dll` was loaded from process start, and the TruckersMP mods were mounted. TruckersMP allows save loading, so every load rule applies to TruckersMP jobs too. `multiplayer.time.offset` was 0 in this session, but about 24,136,737 in the later Sioux Falls TruckersMP session, so it is not a Convoy signal on its own (§6b).

**ETS2 capture** (ETS2 1.61.1.1s, TruckersMP, 2026-10-13 20:18–21:30, one session, plugin `572f0d20`; fixture `ets2-tmp-norway-tunnel-profiles`):
- **TruckersMP SDK verified in the real client.** It called `truckersmp_init` (`loaded`), then reported connected on server 4 with the account ID, and server 30 after a server switch.
  - While reconnecting, `account_id` was briefly 0 with `connected=true`. The server ignores 0.
  - ETS2 module name: `core_ets2mp.dll`.
- **Scales:** city 3, road 19, and **15 on UK roads** (Dover/Folkestone, switching back to 19 in France). Confirmed by you (2026-10-13): the UK in ETS2 is **currently** 1:15, so it may change (for example with a UK rework). It lives in config (`road_local_scales`), not code.
- **Train (Channel Tunnel):** `paused` → `player.use.train` (`tunnel_d` → `tunnel_c`, `pay.amount` 300) → `started`. The position jumps 6.57 world km and the odometer does not change.
- **Bobtail (C23):** on the Dover → Calais cargo-market job, the 113 game km to the pickup were driven with the trailer hooked and no cargo. Without World of Trucks, that is the only way to bobtail (you, 2026-10-13). `distance.km` includes it, and the counted distance excludes it.
- **`distance.km` leaves out train crossings** (Dover → Calais: 239 reported vs 238.87 tracked without the tunnel). It is reported in whole km: a 1 km quick job tracked 1.8 km. The cross-check therefore allows 1 km on top of its tolerance (`delivered_distance_resolution_km`).
- **Loading into a delivered job (C6):** delivered at 21:03:43, a manual save (type 5) was loaded seven seconds later, and the job was delivered again with `delivery.time` 984 instead of 981. The timeline counts it once (`Stopped` and `DuplicateDelivery`).
- **Profile switch through a TruckersMP server change:**
  - `exit` to the menu → `New profile selected` → that profile's autosave (type 0) → `timer_restart`.
  - Back again the same way, which restored the first profile's quick job from its autosave; the timeline truncated the 51 points driven after that save.
  - With `game.log` profile detection the switch would not look like a load, but the result is the same here.
- **Quick jobs:** the game log prints `[job selection]`, and the truck and trailer configs change.
  - A quick job is often accepted straight after a load, before any sample. The game first sends the loaded state (the job config empty, the truck full), and the quick job's config arrives later on its own. A job that appears after such a jobless state is tracked from its start, not a `LateStart` (fixed 2026-10-13). A save with a job in progress still sends its job before the truck, so it stays a `LateStart`.
  - The Cagliari → Napoli quick job is a ferry crossing (`player.use.ferry`, `pay.amount` 0).


**Convoy capture** (ETS2 1.61.1.1s, 2026-10-13 21:52–21:56; fixture `ets2-convoy-quick-job`): a Convoy hosted alone, with a quick job accepted right after the start autosave.
- The game log shows `[MP] Session started` / `Lobby created` / `Session running`, and later `Session closing` / `Session closed`.
- `multiplayer.time.offset` was 0 before the session and 3 → 4 → 5 during it. The plugin's `truckersmp` message said `loaded=false`. So the job is Convoy, not TruckersMP or singleplayer.
- **Hosting is not a load:** no configs were re-sent and no `timer_restart` was raised, so the job continued unbroken.
- A job that is partly driven in a Convoy is labelled Convoy.
- Joining someone else's Convoy, and leaving one while still driving, were not captured.

## 4. Wire protocol (plugin → client)

**Transport**
- The **client listens** on `127.0.0.1:<port>`. The **plugin connects out** from its sender thread.
- The plugin reconnects with backoff and sets `TCP_NODELAY`.
- It always connects to the literal `127.0.0.1`, never to `localhost`.
- Both `WSAEADDRINUSE` (10048) and `WSAEACCES` (10013) mean "port in use".

**Plugin configuration**
- The port is read from `plasmatrack.ini` next to the DLL.
- Flatpak Steam cannot see `~/.config`, so the client writes this ini into every Steam library it finds, including `~/.var/app/com.valvesoftware.Steam/...`.
- If the ini is missing, the plugin uses a fixed default port with a small probe range.

**Framing**
- Each message is a `u32` little-endian length followed by a UTF-8 JSON object.
- Every message carries `{v, sess, seq, sim_us, wall_ms, kind, ...}`:
  - `sess` is a random 128-bit ID generated at each `scs_telemetry_init`.
  - `seq` increases monotonically.

**Message kinds**
- `hello`: game_id, game_version, api_version, plugin version, sample rate, drop counters.
- `snapshot`: all cached configs plus paused state plus latest channel values. Sent on every connect.
- `config`: a raw passthrough of every attribute as `{name, index, type, value}`. Empty configs are included.
- `gameplay`: every event, including undocumented ids. If current game builds emit something like a refuel event, it will show up here.
- `paused` / `started` / `timer_restart`.
- `sample`: raw SDK units, sent at `sample_rate_hz` (default 20) on paused-simulation time. Only fresh channel values are marked `live`.
- `heartbeat`: once per second, also while paused, with the current `local_scale`.
- `scale`: every change of `local.scale` (durable).

**Threading**
- SDK callbacks run on the game main thread (SDK readme). They copy values, strings included, into a per-frame record. At `frame_end` the record is pushed into a lock-free **SPSC ring**.
- If the ring is full, the frame is dropped and a counter is incremented. The game thread never blocks.
- The sender thread owns the socket. It never calls any SDK function, including `log`. Log lines go back through a second SPSC ring and are flushed from a callback.
- `scs_telemetry_shutdown` fully joins the thread, because the DLL may unload right after (`sdk reload`).

**Durability** (red-team finding)
- `config`, `gameplay`, `paused`/`started` and `snapshot` messages go to an **on-disk journal** next to the DLL until the client acknowledges them. Only samples are memory-only, kept in a 120 s replay buffer.
- A client that was closed during a delivery still learns about the delivery when it next connects.
- The client sends a single message type, `ack{sess, seq}`.

## 5. Save loads and the job timeline (`pt-core::timeline`)

This model incorporates the red-team findings. All thresholds are in `[timeline]` and are **provisional until real captures confirm them** (§9).

**Principle.** The tracker follows the game's own timeline. A reverted stretch does not exist, so its distance, fuel and overspeed are never scored. That includes a speed spike from falling under the map, which **clears an Invalid status** once it is reverted.

**Load classification** (revised after the first capture, §3). "Never a single signal" is implemented as follows:
- **World reload (W):** within one bracket, the `job` *and* `truck` configs are both re-sent **empty**, then re-sent (full or empty). All six captured loads showed W. Job accept, cargo loading, cancel and the ferry re-sent configs without emptying `truck`, so they never produce W.
- **Hard rollback evidence (R)** is one of:
  - the odometer decreases beyond ε on the same truck;
  - `game.time` decreases by at least 1 minute (**singleplayer only**; on TruckersMP the clock is server-side, so this signal is ignored);
  - a position discontinuity not explained by a ferry or train event.
- **Session change (S):** a new plugin session (game restart or crash), a data gap, or `timer_restart`.
- **A load** is either:
  - **W** (in-session loads, backwards or forwards); or
  - **S plus a discontinuity** (odometer, position or fuel, plus `game.time` in singleplayer only) against the last kept state (loads across a restart, a crash or a client outage).
- **A discontinuity without W or S is not a load.** It is a **teleport**, such as the two uncaptioned teleports in the capture.
  - **Decided 2026-09-28: a teleport only adds no distance.** It never invalidates, never penalises (not even partly) and never sends a job to review. Examples are F7, ferries, trains, quick travel and **developer-mode (free-camera) teleports**. Developer mode or the console being enabled is not a review reason either (you, 2026-09-28).
  - The odometer does not move on a teleport (first capture), so the tracker's own figure already excludes it.
  - If a delivery capture shows that the game's `job.delivered` `distance.km` *does* count a teleport, the jumped distance is subtracted from the game's figure **before** the delivered-vs-trace cross-check. A teleport therefore never causes a mismatch or a review by itself.
  - It reverts nothing, and it is stored in the trace for admins to see.
- **A pause bracket alone is not evidence.** In the capture, the teleports happened inside pauses, so a pause plus a position jump would otherwise look like a load.
- **Other config changes never count as evidence.** A job or truck config that is re-sent without emptying `truck` only decides which case below applies. This stops ordinary job accepts, cargo loading, quick-job teleports and deliveries from looking like loads.
- **Provisional:** W must still be confirmed not to occur on a quick job (C11). Truck purchases (C12) are separate from job logging and are not captured (you, 2026-10-13). Call for help (C13, F7) was captured without it. If one of them does produce W, the rows below still match against the kept timeline. A truck change itself never sends the job to review (see "Truck changes" below).
- Signals are correlated per **bracket**: from the last live sample before the pause, restart or gap to the first live sample after it. They are not correlated per time window, so a player who waits in the menu after loading is handled correctly.
- `job.delivered` and `job.cancelled` are attributed to the last known job. The job config empties before or around these events (event.h:82-89; RenCloud changelog).

**Job identity.** The fingerprint is: game, `cargo.id`, source and destination city and company ids, the absolute `delivery.time` deadline, `income`, `cargo.mass`, `cargo.unit.count`, `planned_distance.km`, `job.market`, `is.special.job`. Values are typed, and a missing attribute is encoded as absent. Captures must confirm that the fingerprint is byte-identical across loads.

The fingerprint has two parts:
- **Route key:** game, source and destination city and company ids, the `delivery.time` deadline, `planned_distance.km`, `job.market`, `is.special.job`.
- **Cargo key:** `cargo.id`, `cargo.mass`, `cargo.unit.count`, `income`.

**Truck changes (decided; "should not do anything", 2026-09-28):** a truck change has **no effect** on the job, however it happens: truck switch, purchase, quick job, or a save edit (C20).
- The truck is not part of the fingerprint.
- A truck change never counts as load evidence, never opens a gap, never sends a job to review and never raises a rule.
- Distance is measured per truck segment as an odometer difference, so the new truck's odometer never counts as distance driven.
- When a load brings in a different truck, matching against the kept timeline uses **position** (plus game time in singleplayer) and skips odometer and fuel, which belong to the old truck. Tracking continues on a new segment.

**Cargo switch (decided):** a job config whose route key matches the current or a suspended job but whose cargo key differs is a `cargo_switch`, never a new job. The game offers no way to change cargo mid-job, so this can only come from a save edit.
- The job keeps its identity and goes to manual review with reason `cargo_switch`.
- Until an admin decides, the bonus (rule 7) is evaluated on the cargo the job was first tracked with.
- Both cargo keys are kept as evidence for the admin.

**States.** Tracking, Suspended, Delivered, Cancelled, Stopped (replay of a delivered job).

**Kept timeline.** An ordered list of tracked runs and gaps. Reverted material is kept in the trace and marked `reverted_by_save_load(load wall/sim time)`.

| Situation | Result |
|---|---|
| Job appears in continuous, gap-free data | Tracked start. Start odometer and fuel are taken at the first live sample. |
| Job first seen mid-way (plugin snapshot, after a data gap, or loaded save of an unseen job) | **Late start.** Record the details reported so far and track from that point. Distance = `job.delivered` `distance.km`, the same source as every other job (§6). It is converted to 1:1 with the world/game ratio of the part that was tracked, and only that part can be cross-checked against the trace. Goes to review (`review_policy.late_start`). |
| Load, same job, loaded point inside the kept timeline | Truncate to the matching point. Matching uses odometer, position and fuel together, plus game time in singleplayer only (position only if the truck changed). Everything after it, gaps included, is reverted. Recording continues. |
| Load, same job, loaded point matches **previously reverted** material | Restore that branch up to the loaded point and score it normally, overspeed included. This closes the "revert, then restore" exploit, where overspeed would otherwise be laundered into a gap. |
| Load, same job, loaded point beyond anything seen | **Gap** from the last kept point to the loaded point. Resume. Goes to review (`review_policy.gap`). |
| Load to a no-job save or a different job's save | Current job → **Suspended**: not cancelled, not sent to review, no penalty. The other job is resumed (if suspended), tracked (if new, as a late start), or stopped (if already delivered). |
| Suspended job's fingerprint comes back | Resume, using the three "Load, same job" rows above. |
| Load brings back a **cancelled** job (decided) | Resume it; the cancellation is reverted. Exception: if that fingerprint was ever delivered, stop tracking. |
| Load into an **already delivered** job | **Stop tracking** until the fingerprint changes. The server also rejects a second delivery of the same fingerprint for the same player. **Exception (decided): a delivery that was Invalid is reopened**, because the invalidity may come from falling through the map. The Invalid delivery is marked `reverted_by_save_load`, the job resumes, and a later delivery replaces it. |
| Client restart | The plugin's journal and snapshot are compared with SQLite's last-seen state, then the same rows apply. |
| Game crash | The new plugin session is context. The loaded autosave is handled by the same rows. |

**Load paths covered:** quick-load, autosave, main menu, load after a crash, and a client restart on a different save.

**Implementation notes (stage 3, `pt-core::timeline`; from replaying the real captures):**
- A **bracket** is everything between two consecutive samples.
- **W counts only within a running session.** At `scs_telemetry_init` every config starts out empty, so W at session start would turn every game start into a load. A profile switch or a main-menu load keeps the session and raises `timer_restart` alongside W, so it is still a load.
- **A position jump is a teleport only if driving could not cover it** in the elapsed simulation time, at up to 100 world m/s. Samples can go missing while the truck keeps driving (client closed, plugin replay buffer overrun).
- **Bridge gaps.** Missing samples with plausible driving in between still count as distance, as does the odometer difference across a game restart with no discontinuity. They never go to review. Only forward-load gaps do.
- **Across a teleport, distance carries on** (the odometer does not move), but **overspeed episodes end** there.
- **Gaps undone by a later load stop counting:** a gap is active only while its far end is on the kept timeline.
- **`job.delivered` and `job.cancelled` apply the moment they arrive.** The game may stop before another sample.
- **A delivered job keeps its result** after a load stops its tracking.
- **The 1:1 ratio comes from driven stretches only.** A gap carries a single scale reading, and a save-edited one would distort it.
- **Load matching:**
  - the loaded point must lie within 50 world m of a kept point;
  - with the same truck, fuel must be within 1 L and the odometer within 1 game km;
  - in singleplayer, game time must be within 2 minutes;
  - the closest candidate wins, and the latest wins a tie.

**No rule ever fires because of a load:**
- Rule 1 is evaluated on the kept timeline.
- Rule 4 is summed per tracked run, so it never compares fuel across a splice or gap. Loading forward across a refuel therefore raises no flag.
- The optional heuristics ignore discontinuities that were classified as loads.

**Rule fixes from the red team:**
- The overspeed window is measured in `paused_simulation_time` and continues across a pause bracket. Pressing Esc repeatedly no longer resets it.
- Distance is always an odometer difference over a segment, never a sum of per-sample deltas, because the odometer is float32 (about 31 m steps above 262k km).

## 6. Scoring (`pt-core::rules`, precedence as specified)

**Conversions:** mi = km × 0.621371; mph = |m/s| × 2.23694.

**Comparison precision:** values are quantized to `compare_precision_*` before comparing. So exactly 105.0 mph is not over 105, and 150.0 km is not over 150.

**Two distances per job (decided 2026-09-28):**
- **World distance (scored):** the physical 1:1 distance the truck moved through the 3D world, like real-life driving (an hour at 90 km/h is 90 km).
  - **Source (decided 2026-09-28):** the game's own figure at the delivery point, the `distance.km` attribute of `job.delivered` (gameplay_events.h:106-111).
  - **Measured (clean Chicago → Chicago delivery, 2026-09-28): `distance.km` is in GAME scale, not 1:1.**
    - The game reported 71. The odometer measured 70.72 km loaded (71.47 km including the 0.75 km empty leg); the 1:1 distance was 5.93 km loaded.
    - The value appears rounded to whole km (71; 1000 in the edited run).
    - **It includes the empty drive to the pickup** (measured on a clean TruckersMP job, 2026-09-28: dynamite, Chicago → Sioux Falls, planned 1042 km).
      - The game reported **1233**. The tracked kept timeline was 158.85 km empty plus 1073.29 km loaded, 1232.14 km in total (0.07% off). The 1:1 figures were 10.29 + 58.09 km.
      - The job spanned a game restart; the odometer carried over, so there was no discontinuity. It also had an in-game autosave load that undid 3.5 km, and the game left those out too: it rolls its own counter back with the save.
      - So the game's figure is compared with the **whole kept job** (accept → delivery), and the uncounted legs are then subtracted (decision 21).
      - The figure is whole km and can differ from the odometer by up to about 1 km.
  - **So the scored 1:1 figure is the game's figure, minus the uncounted legs, converted with the job's own measured scale mix:**
    - counted game = `distance.km` − tracked uncounted game distance (empty and bobtail legs; none for World of Trucks);
    - world = counted game × (tracked counted world ÷ tracked counted game).
    - Sioux Falls example: (1233 − 158.85) × 58.085 / 1073.29 = 58.13 km = 36.1 mi → **36.1 points**, shown as 667 mi in game.
    - Tracked world = Σ (Δodometer ÷ `local.scale`), where each Δodometer is taken over a run of kept samples with the same `local.scale`; tracked game = Σ Δodometer.
    - Chicago example: (71 − 0.75) × 5.931 / 70.718 = 5.89 km = 3.66 mi → 3.6 points, shown as 44 mi in game.
    - Under a 1:1 mod the scale is 1, so world = game.
  - **Trace cross-check** (same units, game scale): `distance.km` against the tracked **whole-job** game distance on the odometer, from accept to delivery over the kept timeline, before the uncounted legs are subtracted.
    - The first capture shows this matches the actual movement exactly: the ratio is 2.98 at scale 3 and 20.00 at scale 20.
    - If the game does not provide `local.scale`, which only a 1:1 map would do (common_channels.h:14-19), the scale is 1.
    - **ETS2 uses 1:19 on open roads** (you, 2026-09-28), against 20 in ATS. Nothing is hard-coded: the reported `local.scale` is used per run. The first ETS2 capture should show 19 on roads (and presumably 3 in cities).
    - If `distance.km` and the tracked game distance disagree by more than the greater of 1% and 30 mi, **hard-capped at 5% for jobs under 100 mi**, all measured in game miles on the SDK-reported delivery distance (`delivered_vs_trace_*`; PROVISIONAL), the job goes to **manual review** with both values shown. Measured: Chicago 71 vs 71.47 km (0.7%), Sioux Falls 1233 vs 1232.14 km (0.07%), both well inside the tolerance. This is a trace check (§6a), so a save edit or a patched game cannot quietly inflate the figure.
  - Save loads need no special handling: the game restores its own job distance with the save, which matches the kept timeline.
- **Game distance (shown, not scored):** the odometer difference, meaning what the truck's odometer, the job list and the in-game map show. The bot shows both values: "12.3 mi driven (1:1), 146 mi in game".
- Both distances count **loaded, hitched** driving only (decided 2026-09-28): kept segments where the job config has `cargo.loaded=true` **and** `trailer.0.connected` is true.
  - `trailer.0.connected` is the first trailer, the one hitched to the truck (trailer_common_channels.h:16-36). The plugin samples it from this change on; earlier captures lack it.
  - So neither the empty drive to the pickup nor **bobtailing** (driving with the trailer unhooked) earns anything.
  - **Exception: World of Trucks jobs** (`job.market` = `external_contracts` / `external_market`, configs.h:633-637). For them, all driving during the job counts, empty and bobtail included.
  - If the game's `distance.km` turns out to include uncounted legs, the tracked uncounted distance is subtracted before scoring.

**Which rules use which distance:**
- The empty drive to the pickup earns nothing. It happens with an owned trailer on the cargo market; the first capture shows `cargo.loaded` flipping at the pickup.
- Rules 1, 5 and 6 use the loaded **world** distance. The speed rules were already 1:1: `truck.speed` is world m/s, which is what the speedometer shows.
- Rule 3 uses the loaded **world** distance too, with its threshold rescaled for 1:1: over **150 world km** (`manual_review_over_world_km`) goes to review. It replaces the spec's 3000 mi (decided 2026-09-28).
- Speed rules 2 and 5 and fuel rule 4 still watch the **whole job**, from accept to delivery, empty leg included. (This is my default.)
- World of Trucks jobs count their empty and bobtail driving too (see above).

**Rules**
0. **Integrity** (added at your request; runs first; overrides the spec's "no other auto-invalidation"). The job is **discarded** (0 pts, reason and diff visible to admins, not approvable) if either check fails (§6a):
   - the uploaded config's integrity hash matches no server-published config version;
   - the uploaded install manifest matches no published release.
1. Negative kept-timeline distance → Discard.
2. Sustained speed over `invalid_over_mph` → Invalid.
3. Loaded world distance over 150 km → Pending review (was 3000 mi before scoring moved to 1:1).
   - **Average pace** (you, 2026-10-14): the whole job averaging over **5 world km per minute** (300 km/h) → Pending review. It is checked on two clocks in tandem:
     - real time: unpaused simulation time, so pausing cannot lower it (`fast_average`);
     - the game's clock: in-game minutes turned back into real minutes at the game's time rate, which is the map scale (`local.scale`) in singleplayer and 6 on TruckersMP, where one in-game minute is 10 real seconds (`fast_average_game`).

     Distance is odometer ÷ `local.scale`, so ferries, trains and teleports add nothing. Jobs under 1 real minute or 20 in-game minutes of driving are not averaged. The captured jobs peak at 1.52 km/min (91 km/h).
4. Fuel used below −tolerance → **manual review** (points held). Also send every mid-run fuel jump outside a detected refuel to manual review, with its timestamp.
5. Sustained speed over `penalty_over_mph` → accepted distance = 0.85 × driven.
6. 1 point per accepted mile.
7. Bonus cargo (decided): among all matching bonuses, **only the greatest applies**, meaning the one that yields the most points. It stacks with the 0.85× speed penalty: bonus applied to the accepted distance. Flat and multiplier bonuses never stack with each other. The date window is checked at delivery time on the server clock.
- Final points = the total floored to `points_decimals` places, 1 by default: 3.79 → 3.7, never rounded up (decided 2026-09-28, replacing the spec's whole-number floor).

**Evaluation**
- **Overspeed:** eligible samples exclude paused time and ferry/train transfer windows. The record keeps peak mph and time over each threshold.
- **Refuel (decided):** the SDK has no refuel event, so a refuel is a **heuristic** episode: the fuel rises while the truck is stationary, at a plausible rate (`[rules.refuel]`). Only litres are tracked. Fuel cost is never calculated.
- **Fuel across save loads (decided):** a fuel change across a load is most likely a save edit, and it is **ignored**. There's no flag and no rule 4, because fuel is only summed within tracked runs.
- **Fuel across teleports (2026-10-15, found by you):** ignored the same way. World of Trucks event portals (Brackenreach, likely Winterland too) fill the tank as they move the truck, which had raised `fuel_jump` and `negative_fuel`. The fuel used shown for a job leaves it out as well.
- **Manual review everywhere (decided): every "flag" means manual review**, with the job pending and its points held until an admin approves or rejects it via the Discord bot. `review_policy` values are `review` or `off`. Gap, late-start and trace checks default to `review`.
- **Other heuristics:** odometer jump and impossible delivery time exist only as manual-review triggers, **off by default**. Teleports are never a trigger (§5). Diagnostics (unexplained Δodometer, derived vs. reported speed, fuel against capacity) are always *stored* for admins but never trigger review.
- **Record:** every rule result is stored, and a job can carry any number of flags.
- **Recalculation:** each job records the config version and bonus set it was scored with. Admin recalculation replays the raw trace.

## 6a. Integrity: config and install files

**Config**
- Every plugin session's upload carries the client's full effective config and its `integrity_hash`.
- The hash is SHA-256 of canonical sorted-key JSON of everything except `[local]`. It is implemented in `pt-core::config` and tested.
- `[local]` holds `listen_address`, `port` and `server_url`. These may differ per driver and are the only keys excluded. Every other key must match.
- The server checks the hash against `config_versions`:
  - current version: OK;
  - any earlier published version: OK, recorded as outdated, and the client is told to update;
  - no match: **Discard (rule 0)**.
- The job stores the hash, the matched version and a key-by-key diff against the current version.

**Install files**
- Every session upload carries a **manifest**: SHA-256 of every installed PlasmaTrack file.
  - the plugin DLL/.so found in each Steam library, including Flatpak paths;
  - the client binary;
  - the config file.
- The plugin also hashes its own module file and reports it in `hello`. That hash must agree with the manifest.
- Release builds produce a manifest, which an admin publishes on the server (`release_manifests`).
- Any file that matches no published release, a missing or extra PlasmaTrack file, or a disagreement between the plugin's self-hash and the manifest → **Discard (rule 0)**.

**Limit.** A modified client can upload clean hashes while running tampered code, so rule 0 raises the cost of cheating but cannot prove integrity. The server therefore also measures the trace itself: real sample intervals against the declared `sample_rate_hz`, sequence continuity, and whether `hello` agrees with the data. These trace checks send the job to **manual review (decided), not discard**. At low FPS the game legitimately delivers fewer samples than configured (they are emitted at frame end), so discarding on them would lose honest jobs.

## 6b. Profiles, multiplayer and notifications (decided)

**Profiles are separate timelines.** Different profiles have different jobs, trucks and progress, so all timeline state (§5) is keyed by (player, game, **profile_key**).
- A profile switch is never treated as a save load within one timeline. The job of the profile you left stays suspended in its own timeline.
- **The SDK has no profile identifier** (§3), so `profile_key` comes from the game's own log (decided 2026-10-14, from the ATS and ETS2 captures):
  - Every world load writes `Loading save. … path: …/(home|steam)/profiles/<HEX>/save/…` to `Documents/<game>/game.log.txt`. `<HEX>` is the profile name in hexadecimal.
  - The key is 16 hex digits of SHA-256 over `"{home|steam}/{HEX}"`, so **the profile name never leaves the PC**, and a local and a Steam Cloud profile of the same name differ.
  - The client follows both games' logs (under Proton: inside each library's `compatdata` prefix). At each `Loading save` it writes a `profile` note into the running session, placed after the newest plugin frame (`docs/UPLOAD_API.md`).
  - The note reaches the timeline inside the load's bracket, before the first sample of the loaded world: loading takes seconds and the log is read every 250 ms.
- Frames before the first note go to the `unknown` profile.

**Multiplayer is tracked** in every form: TruckersMP, other multiplayer mods, and Convoy.
- **Convoy** is detected from `multiplayer.time.offset ≠ 0` (common_channels.h:287-300) **while no TruckersMP module is loaded**. The Sioux Falls TruckersMP session reported an offset of about 24,136,737, while the first TruckersMP capture reported 0, so the offset alone does not mean Convoy.
- **TruckersMP (decided 2026-09-28: use the TruckersMP client SDK).** `plasmatrack.dll` is also a TruckersMP client plugin.
  - It exports `truckersmp_init` / `truckersmp_shutdown` (vendored SDK 1.1.0, MIT, C layer only).
  - It reports `truckersmp` messages: `connected` (`TruckersMP_Network::IsConnected`, updated on the connect/disconnect events), `server_id` and `account_id`.
  - **`account_id` is uploaded** (decision 24): the server records which TruckersMP accounts each player drove on, and the bot uses it to link TruckersMP profiles.
  - **A job counts as TruckersMP only while connected to a server.** Singleplayer launched through the TruckersMP launcher loads `core_atsmp.dll` but is not TruckersMP.
  - Fallback for plugins without the SDK side: a module named in `[multiplayer] truckersmp_module_names` is loaded (confirmed for ATS: `core_atsmp.dll`).
  - TruckersMP supports an SDK version for about a year, so the plugin must be rebuilt against newer headers yearly.
  - Not yet verified in the real TruckersMP client: that it loads our DLL as a plugin next to the telemetry role, and what `server_id` values look like.
- **Singleplayer Division:** a user flag, set through the admin API. For these users, jobs detected as TruckersMP are **discarded**, with the reason shown. Convoy and other multiplayer remain allowed.
- Each job stores its multiplayer kind.

**Player notifications** (desktop notification from the client: a notification from its tray icon, which Windows 10/11 shows as a toast; no extra dependency. The console client and Linux only log them):
- Sent when a delivery is marked **Invalid**.
- Sent when the **0.85× speed penalty activates** during a job.
- Each notification fires after a uniformly random delay between 5 and 30 s, drawn at microsecond resolution from the OS random source (`[notifications]`).
- These are provisional client-side results. The server's result is authoritative.
- Windows focus assist / "do not disturb while gaming" can hold toasts until you leave the fullscreen game.

## 7. Data model (server SQLite; the client mirrors raw frames, jobs and upload queue)

| Table | Contents |
|---|---|
| `users` | id, name, role (player/admin), token_sha256, discord_user_id, **singleplayer_division** flag, disabled |
| `plugin_sessions` | sess, user, game, game_version, first/last seq |
| `raw_chunks` | sess, seq_from, seq_to, zstd blob, received_at. Append-only and idempotent on (sess, seq) |
| `jobs` | user, **game**, fingerprint, state, start_kind (tracked/late_start), config snapshot fields, truck_key, start/end odometer and fuel, refuelled_l, delivered_distance_km (game's job.delivered figure, scored source), trace_world_mi (odometer ÷ local.scale, cross-check), world_driven_mi (scored, 1:1), world_accepted_mi, game_driven_mi (odometer, shown), points, peak mph, seconds over each threshold, delivered attributes, outcome (discarded / invalid / pending_review / valid / approved / rejected / voided), review_reasons[], multiplayer (none / convoy / truckersmp / other), profile_key, config_version |
| `job_segments` | kind (tracked / gap / reverted / restored), seq range, load timestamps |
| `rule_results` | job, rule, triggered, details JSON |
| `bonus_cargo` | game, match (exact or glob), type (flat/multiplier), amount, active_from/to |
| `audit_log` | actor, action, target, **required reason**, before/after JSON, time |
| `config_versions` | TOML text, integrity hash, author, time, current flag |
| `release_manifests` | release version, platform, file path pattern, SHA-256, published_at, revoked flag |
| `job_integrity` | job, config hash, matched config version (or none), config diff JSON, manifest JSON, manifest result, trace-check results |

**Uploads**
- The client uploads raw chunks to `POST /api/v1/chunks` with its bearer token.
- Chunks are queued in SQLite. Failures retry every `sync.retry_interval_wall_s` (20 s) until they succeed.
- The server derives all jobs itself. A client-computed result is never an input.

## 7a. The server on Cloudflare (stage 4; decision 15)

Everything runs on Cloudflare Workers Paid. Limits that shape the design (checked 2026-10-14,
developers.cloudflare.com): 30 s CPU per request (up to 5 min), **128 MB memory per isolate**,
D1 rows ≤ 2 MB and 10 GB per database, Queue messages ≤ 128 KB with delays ≤ 24 h and
retention ≤ 14 days.

**Bindings**

| Binding | Holds |
|---|---|
| D1 `DB` | every table in §7 except the raw data; idempotency keys; the webhook outbox |
| R2 `RAW` | raw upload chunks (`raw/{user}/{sess}/{seq_from}.jsonl.gz`), each scored job's points (`jobs/{job_id}.json.gz`, for the series, trace and route endpoints), timeline snapshots (`snap/{user}/{game}.json.gz`) |
| Durable Object `Scorer` | one per (player, game): the only place that runs the timeline, so a player's frames are processed strictly in order, one batch at a time |
| Queue `webhooks` | event ids to deliver; the body stays in the D1 outbox (bodies can be 256 KiB) |
| Queue `ops` | bulk recalculation: one message per (player, game) |
| Cron | every 5 min: revocations that took effect, expired idempotency keys and cursors |

**Upload path**
1. `POST /api/v1/chunks`: the Worker checks the token, then writes the chunk to R2 and records
   it in D1.
2. It answers `200` with the ack straight away; scoring never delays the client.
3. It then pokes the player's `Scorer`.

**Scoring path**
1. The `Scorer` keeps the player's `pt_core::timeline::Profiles` in memory. After a restart it
   loads the snapshot from R2 and replays the chunks uploaded since.
2. Each batch pushes the new frames. Every job whose state changed is evaluated with
   `pt_core::rules`, then written to D1 (and its points to R2).
3. The webhook events are emitted: `job.needs_review`, `job.scored`, `job.discarded`, and
   `player.notification` with its random 5–30 s delay as the Queue delay.
4. The snapshot is written back once the player has been idle for a minute, and at least
   every 10 minutes.
5. **Out-of-order data** (a chunk for a session older than the snapshot's position) rebuilds
   that (player, game) from the raw chunks. A recalculation does the same.

**Bounded memory (decided 2026-10-14).** Samples arrive at 20 Hz, so a timeline that kept every
job forever would outgrow 128 MB.
- When a new session starts, a profile timeline drops jobs that finished (delivered,
  cancelled or stopped) more than `[timeline] reopen_window_wall_h` (48 h) before it.
- Those jobs are already stored, so nothing is lost. The only effect: a save from such a job
  loaded *after* the window is no longer recognised as that job. Its driving then starts a
  new late-start job, which goes to manual review.
- The window is measured on frame wall-clock times, never on the server's clock. A full
  replay therefore reaches exactly the same result as incremental scoring.
- The client applies the same rule to its own provisional timeline.

**Contract tests.** `pt-server` is exercised natively: a rusqlite database runs the same SQL
migrations as D1, R2 is an in-memory map, and the clock and RNG are injected. Every example
in the four contract files is checked against the real responses.

## 8. Server APIs (all consumed by the Discord bot, a separate repo; there is no website)

This repo ships **no HTML**. The server exposes versioned JSON APIs, and two separate repos consume them:
- the **Discord bot repo**: everything user-facing. That covers public job info, per-job route maps and leaderboards, plus all admin actions.
- the **Discord bot repo**: all admin actions.

The contracts are `docs/API_CONVENTIONS.md`, `docs/PUBLIC_API.md`, `docs/ADMIN_API.md` and `docs/WEBHOOKS.md` (v1 draft 2026-10-14, written against the bot's `docs/CONTRACT_REQUIREMENTS.md`). OpenAPI and example payloads follow in stage 4, with contract tests.

**Public read API** (`/api/v1/public/...`, authenticated with the bot service token; no browser CORS needed):
- Per-game job list and leaderboard: `/ets2/...` and `/ats/...`. Games are never combined.
- Job detail: stats, outcome, rule results that are safe to show publicly.
- **Route as GeoJSON** in lon/lat, already projected, split into LineStrings at ferries, trains, pauses and gaps.
- **Scale colouring (decided 2026-09-28):** the route is also split at every `scale` change. Each LineString carries `local_scale` and a `scale_class`, so the bot can colour them for moderators:
  - `city` = 3;
  - `road` = a vanilla road scale (ATS 20; ETS2 19, and 15 on UK roads);
  - `one_to_one` = 1, a 1:1 mod;
  - `nonstandard` = any other value, a modded scale;
  - `unknown` = not reported.
  
  Suggested colours: city grey, road blue, one_to_one orange, nonstandard red, unknown dashed. The vanilla values per game live in config, not in code.
- **Modded maps are drawn anyway.** Routes are always overlaid on the vanilla base map, even when the coordinates do not line up. A misaligned route is itself the visible sign of a 1:1 or modded map. Scale never sends a job to review on its own; moderators see it on the map and in the job detail (distance per `scale_class`).
- Reverted material is never exposed publicly.
- **TruckersMP accounts (decision 24):**
  - Per player: every TruckersMP `account_id` seen while connected to a server, each with first seen, last seen and job count.
  - Per job: `multiplayer` (`singleplayer` / `truckersmp` / `convoy`), and for TruckersMP the `account_id` and `server_id`.
  - `0` means unknown and is never listed.
  - The admin API adds two flags for moderators:
    - a player whose jobs come from more than one TruckersMP account;
    - a TruckersMP account that appears under more than one player.

**Route projection** stays in this repo (`pt-core::projection`, server-side), so the bot only draws GeoJSON.
- Game world X/Z (metres) → lon/lat. Implemented independently from the game's `def/climate.sii` parameters and standard Lambert conformal conic maths. No GPL code is copied; truckermudgeon is credited for the ETS2 UK correction.
- ATS: inverse LCC (lat₁ 33, lat₂ 45, lat₀ 39, lon₀ −96).
- ETS2: offset (−16660, −4150), a UK box scaled 0.75 around (15550, 2750), then LCC (37, 65, 50, 15).
- Unit tests use four known points (Paris, Crete, London, Las Vegas).

**Admin API for the Discord bot** (`/api/v1/admin/...`):
- review queue: every job pending manual review (with its reasons), plus invalid jobs;
- job detail with every rule result, the full trace (reverted parts marked), and speed/fuel series and route GeoJSON, so the bot can chart and map them;
- approve, reject or void, each with a **required reason**;
- bonus cargo CRUD;
- recalculate one job or all jobs;
- issue and revoke player tokens;
- publish config versions and release manifests (rule 0), and schedule revocations (effective 21 days after announcement).
- **Auth:** a bot service token plus the acting admin's Discord user ID on every call. That ID goes into the audit log.
- **Webhooks:** signed POSTs (`job.needs_review`, `job.scored`, `job.discarded`, `player.notification`, `version.revocation_scheduled`, `version.revocation_cancelled`, `operation.completed`) to one URL set in the server config (`docs/WEBHOOKS.md`).
- **Gap and late-start jobs are held for admin approval** (`review_policy = "review"`).

**Map basemap (decided; built and used in the bot repo):**
- You generate ETS2 and ATS vector tiles (PMTiles) with truckermudgeon/maps (GPL-3.0) from your own game install. This is allowed for modding as long as nothing is sold, so **PlasmaTrack must stay non-commercial**.
- Discord cannot show interactive maps, so the **bot renders each route to an image**, using your PMTiles and the route GeoJSON from this server. No hotlinking.
- Before publishing: strip features flagged hidden or secret, and credit SCS Software (not affiliated), truckermudgeon/maps, MapLibre and PMTiles.
## 9. Verification that needs your game install

I cannot run the games in this session, so the following steps need a real game install. The research found ETS2 and ATS installed under `C:\Program Files (x86)\Steam\steamapps\common` on this PC.

- **Capture build.** A plugin mode that logs every callback verbatim.
- **Capture protocol** (`docs/CAPTURE.md`), for each game:
  - quick-save followed by quick-load
  - autosave load
  - load from the main menu
  - kill the game mid-job, then restart
  - profile switch
  - delivery (event ordering)
  - cancel
  - ferry and train
  - refuel
  - quick-job accept (teleport plus truck swap)
  - truck purchase
  - repair/service teleport
  - profile switch with a job active in each profile (to confirm the source of `profile_key`)
  - a TruckersMP session (to confirm its module names) and a Convoy session
- **Use of the captures.** They become fixtures and calibrate every PROVISIONAL value.
- **Loopback check.** A `loopcheck` DLL/exe plus the steps from the research: compare `/proc/<pid>/ns/net` of the game and wineserver with the host, then run `loopcheck.dll` in the real game.
  - Needs one native-Steam and one Flatpak-Steam Linux box.
  - Evidence from source code: pressure-vessel keeps the host network (steam-runtime-tools `wrap-setup.c:124-125`), and Flatpak Steam shares the network.
  - This has not been run on real hardware yet.
- **Firewall check.** On this PC, check whether Windows Firewall prompts when the client listens on 127.0.0.1. This is unverified.

## 10. Stages (each must build and pass tests before the next)

0. **Foundation.** Done.
1. **Plugin.**
   - Toolchains (llvm-mingw, CMake and Ninja via winget; all FOSS) and the ring buffer.
   - Wire encoder and journal.
   - Fake-game harness tests, plus the `sdk reload` cycle test.
   - DLL import-table check.
   - Capture mode.
2. **Client capture.**
   - Listener, SQLite, snapshot/replay/ack handling, the uploader with 20 s retry, and the status page.
3. **Validation.**
   - `pt-core` timeline and rules, and the `pt-replay` harness.
   - Every fixture: spec boundaries, load scenarios and red-team exploit cases.
   - **Status (2026-09-28): the core is done.**
     - `pt-core::{trace, job, timeline, rules}` are written.
     - `pt-replay` provides the CLI plus `--trim-to` for fixtures.
     - 38 simulated fixtures in `crates/pt-replay/tests/scenarios.rs` cover every bullet below plus decisions 20–23.
     - 4 real-capture regressions in `tests/real_captures.rs` pass (Chicago 3.6 points, Sioux Falls 36.1, six-load TruckersMP session, the tampered 1000 km delivery → review).
     - **Done (2026-10-14):** `profile_key` from `game.log.txt`, live provisional results with the two alerts, and the job list on the status page (`docs/CLIENT.md`).
4. **Server APIs.** Ingest, scoring, rule 0, auth, the public read API, the admin API and webhooks for the Discord bot, audit log, recalculation, and OpenAPI contract tests.
   - **Status (2026-10-14): deployed** on Cloudflare, production and a separate test instance (docs/SERVER.md). `pt-server` has 11 end-to-end tests that upload a real capture through the API. The OpenAPI file and examples are generated from it (`tests/contract.rs`).
5. **Route projection and GeoJSON endpoint.** Rendering route images happens in the bot repo.
   - **Status (2026-10-15): done.** `pt-core::projection` matches truckermudgeon/maps' four
     reference points (Las Vegas, London, Crete, Paris) to 1e-9°; the real Chicago capture lands
     in Chicago. `pt-server::route` builds both GeoJSON endpoints.

**Fixtures.** One fixture per bullet below.

*Spec boundaries*
- 105.0 / 125.0 mph in ATS, and 100.0 / 150.0 mph in ETS2, each exactly on the limit.
- A 1-second speed spike vs. sustained overspeed.
- Overspeed while on a ferry.
- 150 vs. 150.1 world km.
- Negative fuel within tolerance and beyond it.
- A legitimate refuel.
- Negative distance.
- A cancelled job.
- A crash mid-job.
- Bonus cargo combined with the speed penalty.
- An ETS2 job measured in km.

*Save-load scenarios you specified*
- Load forward on the same job, then deliver.
- Load forward across a refuel.
- Load a no-job save, then load forward back into the job.
- Client closed, a future save loaded, client restarted.
- Load forward, then back past the gap.
- Load forward onto a different job's save.
- Load into an already-delivered job.
- A late-start job.
- An Invalid status cleared by a load.
- A delivered Invalid job reopened by a load, then delivered clean.
- Greatest-bonus selection, combined with the 0.85× penalty.
- A Singleplayer Division user on TruckersMP (discarded) vs. on Convoy (scored).
- A profile switch mid-job (separate timelines, nothing suspended across profiles).
- A trace-check mismatch (manual review).
- Negative fuel (manual review, points held).

*Red-team cases*
- Pressing Esc repeatedly during overspeed.
- The revert-then-restore exploit.
- Accepting a job or quick job, which must not be read as a load.
- Delivery ordering.
- A delivery while the client is closed.
- A truck swap.
- Pausing in a menu after a load.

## 11. Decisions (2026-09-27)

1. **Map:** you generate the tiles yourself with truckermudgeon/maps. This is allowed for modding as long as nothing is sold, so PlasmaTrack stays non-commercial. The tiles are rendered in the bot repo (§8).
2. **Refuel:** the heuristic is accepted. Fuel cost is never calculated. Fuel spikes across save loads are treated as save edits and ignored (§6).
3. **Captures:** you will run the capture protocol in both games (§9).
4. **Cancelled job brought back by a load:** resume it, unless that job was already delivered (§5).
5. **Review:** every "flag" means manual review, with points held until an admin approves. This applies to gap, late-start, trace checks and rule 4. All admin actions run through your Discord bot in a separate repo (§6, §8).
6. **Minimum patch:** 1.36. Earlier versions are unsupported. Manual submissions go through the Discord bot (decision 25).
7. **Integrity:** rule 0 discards jobs whose config or install files match no published version. Trace checks send the job to manual review (§6a).
8. **No website:** all user-facing info and admin actions go through the Discord bot (separate repo). This repo exposes the JSON API and projected route GeoJSON only (§8).
9. **Delivered-but-Invalid job brought back by a load:** it is reopened, because falling through the map can cause it (§5).
10. **Notifications:** desktop notifications for an Invalid delivery and for the speed penalty activating, each delayed by a random 5–30 s at µs resolution (§6b).
11. **Multiplayer:** all of it is tracked. For Singleplayer Division users, TruckersMP jobs are discarded (§6b).
12. **Profiles:** separate timelines per profile (§6b).
13. **Bonuses:** only the greatest bonus applies, and it stacks with the 0.85× penalty (§6).
14. **License:** PolyForm Noncommercial 1.0.0 is a **placeholder**. The final license will be settled with the client you are building this for.
15. **Hosting (2026-10-14): Cloudflare**, on your existing Workers Paid plan (§1, §7a).

16. **Notifications also go to Discord DMs.** The server emits a signed `player.notification` webhook (Invalid delivery, speed penalty activated), and the bot DMs the player. The server applies its own random 5–30 s delay before sending it. The server only learns of the penalty once the chunk containing it is uploaded.
17. **Revocation.** Admins can revoke a release or config version, and it takes effect **21 days after it is announced**.
    - When revocation is scheduled, the server emits `version.revocation_scheduled`, and the bot DMs every player whose latest session used that version.
    - Jobs captured before the effective time are unaffected. Jobs captured with the revoked version after it are discarded (rule 0).
18. **DLC-gated map areas:** you own all DLC and will handle this in the tile generation and the bot repo.
19. **Discord bot:** it will be written afterwards by parallel agents against `docs/ADMIN_API.md`, which this repo defines in stage 4.
20. **Truck vs cargo changes (2026-09-28):** switching trucks mid-job is allowed. A mid-job cargo change (same route, different cargo) is treated as a save edit and goes to manual review with reason `cargo_switch`; the bonus uses the original cargo until an admin decides (§5).
21. **Empty driving and bobtailing earn no points (2026-09-28):** only distance driven with `cargo.loaded=true` and the trailer hitched (`trailer.0.connected`) is scored. The exception is World of Trucks jobs, where all job driving counts (§6).
23. **Points keep one decimal (2026-09-28):** totals are floored to 0.1 (`points_decimals`) rather than to whole points.
22. **Count 1:1 (2026-09-28):** points come from the **physical world distance**, the distance the truck actually moved at 1:1 scale, like real driving. This is unique among trackers. The **in-game (odometer) distance is reported as a second value**, so players see both.
    - In the first capture, 79.5 game km equals 7.9 world km.
    - World distance = the game's `job.delivered` `distance.km` at the delivery point. That figure is in game scale (measured), so it is converted to 1:1 with the job's own tracked world/game ratio. The tracked odometer distance cross-checks it, and a mismatch goes to review (§6).
    - Rule 3 moves to world distance too: over **150 world km** goes to review (§6).
    - ETS2 uses 1:19 on roads (1:15 on UK roads) and ATS 1:20; all are read from `local.scale`.
    - **Purpose: it must work with 1:1 scale mods** (you, 2026-09-28). Nothing assumes vanilla scale.
      - The scored distance is the game's own delivery figure.
      - The cross-check divides by whatever `local.scale` the game reports.
      - The distance-tolerance thresholds are measured on the delivery distance.
      
      Decided for 1:1 mods (2026-09-28):
      - **One rule-3 threshold for every map scale.**
      - The plugin reports every scale change (a durable `scale` message, checked each frame, plus the current value in the 1 Hz heartbeat).
      - Route maps colour each stretch by its scale class, overlaid on the vanilla map (§8).

24. **TruckersMP account ID is uploaded (2026-09-28):** the plugin reads it with `TruckersMP_Accounts::GetAccountID`, and it travels in the `truckersmp` message like any other frame. The server stores the account IDs seen per player; the Discord bot uses them to link and show TruckersMP profiles (§8).

25. **Manual jobs and moderator edits (2026-10-15, the bot's proposal, approved by you):**
    - Players submit jobs by hand through the bot, with WebP evidence images. They always go to review.
    - World distance = game distance ÷ map scale (ATS 20, ETS2 19 by default), with the UK stretch at 15 and up to five adjusted sections at their own scales. A city-scale section counts only from 1 real mile (3 mi in game); shorter ones are rejected (2026-10-15).
    - Bonuses apply to manual jobs only when a moderator sets one.
    - Moderators may edit **every** field of any job, tracked or manual. Edits are stored as overrides and survive re-scoring and replays.
    - Contract: PUBLIC_API §6, ADMIN_API §14.

26. **Automatic client updates (2026-10-15, you):** from 0.2.0, the installed client updates
    itself (docs/UPDATES.md).
    - Releases are signed with an offline Ed25519 key, and clients trust only the key built into
      them.
    - Files are served from the R2 bucket `plasmatrack-releases`.
    - Updates install once no game runs, and a failed update rolls back.
    - Only some Discord roles may download the tracker. The bot gets personal one-hour setup
      links, and updates need a player token.
    - Anyone may download the uninstaller (`/dl/uninstall`). Installed copies are in Windows'
      Installed apps.

## 12. Open questions

1. ~~**Route images.**~~ Settled: the bot repo renders all images; this server serves GeoJSON and series only (bot contract requirements, 2026-10-14).

The final license is deferred to you.
