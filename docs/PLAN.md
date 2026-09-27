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
| Server | **Rust**: axum + tokio + **rusqlite (SQLite, WAL)** | Same language and same `pt-core` crate. SQLite keeps it a single binary. **rusqlite on both sides** avoids the `libsqlite3-sys` `links` conflict that sqlx-sqlite + rusqlite hit in one workspace. Postgres remains possible later. |
| Shared | `pt-core`: pure, with no I/O, no clock and no RNG | Deterministic. Client and server produce bit-identical results. The replay harness uses it directly. |
| Web | Server-rendered HTML + no-build vendored ES modules. **Leaflet** (BSD-2), **uPlot** (MIT) for speed/fuel charts | No Node toolchain needed. Everything is FOSS. |
| Auth | Admins: argon2id password + session cookie. Players: 256-bit server-issued token, stored as SHA-256 | As specified. |

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
crates/pt-server/          binary: ingest API, scoring, public pages, admin panel
fixtures/                  *.trace.jsonl + *.expected.json (one per rule / boundary / load scenario)
web/                       static assets (map, charts, admin)
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
| `game.time` | u32 in-game minutes | common_channels.h:277-285 | rollback detection |
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
- `heartbeat`: once per second, also while paused.

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

**Load classification.** "Never a single signal" is implemented as follows:
- **Hard rollback evidence (R)** is one of:
  - the odometer decreases beyond ε on the same truck;
  - `game.time` decreases by at least 1 minute;
  - a position discontinuity not explained by a ferry or train event.
- **Independent context (C)** is one of:
  - a re-sent config;
  - a `paused`/`started` bracket;
  - the `timer_restart` flag (the SCS example comments it as "e.g. after load");
  - a plugin session change or a data gap.
- A **load** requires R plus a second independent signal, which may be a C or another R from a different channel.
- **Job and truck config changes never count as evidence.** They only decide which case below applies. This stops ordinary job accepts, quick-job teleports and deliveries from looking like loads.
- Signals are correlated per **bracket**: from the last live sample before the pause, restart or gap to the first live sample after it. They are not correlated per time window, so a player who waits in the menu after loading is handled correctly.
- `job.delivered` and `job.cancelled` are attributed to the last known job. The job config empties before or around these events (event.h:82-89; RenCloud changelog).

**Job identity.** The fingerprint is: game, `cargo.id`, source and destination city and company ids, the absolute `delivery.time` deadline, `income`, `cargo.mass`, `cargo.unit.count`, `planned_distance.km`, `job.market`, `is.special.job`. Values are typed, and a missing attribute is encoded as absent. Captures must confirm that the fingerprint is byte-identical across loads.

**States.** Tracking, Suspended, Delivered, Cancelled, Stopped (replay of a delivered job).

**Kept timeline.** An ordered list of tracked runs and gaps. Reverted material is kept in the trace and marked `reverted_by_save_load(load wall/sim time)`.

| Situation | Result |
|---|---|
| Job appears in continuous, gap-free data | Tracked start. Start odometer and fuel are taken at the first live sample. |
| Job first seen mid-way (plugin snapshot, after a data gap, or loaded save of an unseen job) | **Late start.** Record the details reported so far and track from that point. Driven distance = `job.delivered` `distance.km`. Goes to review (`review_policy.late_start`). |
| Load, same job, loaded point inside the kept timeline | Truncate to the matching point. Matching uses odometer, game time, position and fuel together. Everything after it, gaps included, is reverted. Recording continues. |
| Load, same job, loaded point matches **previously reverted** material | Restore that branch up to the loaded point and score it normally, overspeed included. This closes the "revert, then restore" exploit, where overspeed would otherwise be laundered into a gap. |
| Load, same job, loaded point beyond anything seen | **Gap** from the last kept point to the loaded point. Resume. Goes to review (`review_policy.gap`). |
| Load to a no-job save or a different job's save | Current job → **Suspended**: not cancelled, not flagged, no penalty. The other job is resumed (if suspended), tracked (if new, as a late start), or stopped (if already delivered). |
| Suspended job's fingerprint comes back | Resume, using the three "Load, same job" rows above. |
| Load into an **already delivered** job | **Stop tracking** until the fingerprint changes. The server also rejects a second delivery of the same fingerprint for the same player. |
| Client restart | The plugin's journal and snapshot are compared with SQLite's last-seen state, then the same rows apply. |
| Game crash | The new plugin session is context. The loaded autosave is handled by the same rows. |

**Load paths covered:** quick-load, autosave, main menu, load after a crash, and a client restart on a different save.

**No rule ever fires because of a load:**
- Rule 1 is evaluated on the kept timeline.
- Rule 4 is summed per tracked run, so it never compares fuel across a splice or gap. Loading forward across a refuel therefore raises no flag.
- The optional heuristics ignore discontinuities that were classified as loads.

**Rule fixes from the red team:**
- The overspeed window is measured in `paused_simulation_time` and continues across a pause bracket. Pressing Esc repeatedly no longer resets it.
- Distance is always an odometer difference over a segment, never a sum of per-sample deltas, because the odometer is float32 (about 31 m steps above 262k km).

## 6. Scoring (`pt-core::rules`, precedence as specified)

**Conversions:** mi = km × 0.621371; mph = |m/s| × 2.23694.

**Comparison precision:** values are quantized to `compare_precision_*` before comparing. So exactly 105.0 mph is not over 105, and 3000.0 mi is not over 3000.

**Rules**
1. Negative kept-timeline distance → Discard.
2. Sustained speed over `invalid_over_mph` → Invalid.
3. Driven distance over 3000 mi → Pending review.
4. Fuel used below −tolerance → Flag. Also flag every mid-run fuel jump outside a detected refuel, with its timestamp.
5. Sustained speed over `penalty_over_mph` → accepted distance = 0.85 × driven.
6. 1 point per accepted mile.
7. Bonus cargo.
- Final points = floor(total).

**Evaluation**
- **Overspeed:** eligible samples exclude paused time and ferry/train transfer windows. The record keeps peak mph and time over each threshold.
- **Refuel:** the SDK has no refuel event, so a refuel is a **heuristic** episode: the fuel rises while the truck is stationary, at a plausible rate (`[rules.refuel]`).
- **Late-start and gap review:** applied through `review_policy` (hold, flag or off; default hold).
- **Other heuristics:** teleport, odometer jump and impossible delivery time exist only as review flags, **off by default**. Diagnostics (unexplained Δodometer, derived vs. reported speed, fuel against capacity) are always *stored* for admins but never flag anything.
- **Record:** every rule result is stored, and a job can carry any number of flags.
- **Recalculation:** each job records the config version and bonus set it was scored with. Admin recalculation replays the raw trace.

## 7. Data model (server SQLite; the client mirrors raw frames, jobs and upload queue)

| Table | Contents |
|---|---|
| `users` | id, name, role (player/admin), token_sha256, password_argon2 (admins only), disabled |
| `plugin_sessions` | sess, user, game, game_version, first/last seq |
| `raw_chunks` | sess, seq_from, seq_to, zstd blob, received_at. Append-only and idempotent on (sess, seq) |
| `jobs` | user, **game**, fingerprint, state, start_kind (tracked/late_start), config snapshot fields, truck_key, start/end odometer and fuel, refuelled_l, driven/accepted mi, points, peak mph, seconds over each threshold, delivered attributes, outcome (discarded / invalid / pending / flagged / valid / approved / rejected / voided), config_version |
| `job_segments` | kind (tracked / gap / reverted / restored), seq range, load timestamps |
| `rule_results` | job, rule, triggered, details JSON |
| `bonus_cargo` | game, match (exact or glob), type (flat/multiplier), amount, active_from/to |
| `audit_log` | actor, action, target, **required reason**, before/after JSON, time |
| `config_versions` | TOML text, author, time |

**Uploads**
- The client uploads raw chunks to `POST /api/v1/chunks` with its bearer token.
- Chunks are queued in SQLite. Failures retry every `sync.retry_interval_wall_s` (20 s) until they succeed.
- The server derives all jobs itself. A client-computed result is never an input.

## 8. Public site and admin panel

**Public site**
- Per-game job list and leaderboard at `/ets2/...` and `/ats/...`. Games are never combined.
- **One public page per job**, showing the route on the map, stats and outcome. Reverted material is never shown publicly.

**Route projection.** Game world X/Z (metres) → lon/lat on the server. It is implemented independently from the game's `def/climate.sii` parameters and standard Lambert conformal conic maths. No GPL code is copied. truckermudgeon is credited for the ETS2 UK correction.
- ATS: inverse LCC (lat₁ 33, lat₂ 45, lat₀ 39, lon₀ −96).
- ETS2: offset (−16660, −4150), a UK box scaled 0.75 around (15550, 2750), then LCC (37, 65, 50, 15).
- Unit tests use four known points (Paris, Crete, London, Las Vegas).
- Routes are split at ferries, trains, pauses and gaps.

**Admin panel**
- Bonus cargo create, read, update and delete (CRUD).
- A review queue of pending, flagged, invalid, gap and late-start jobs. Each job shows the full trace (reverted parts greyed), a uPlot speed/fuel chart, the route, and every rule result.
- Actions: approve, reject, void. Each requires a reason and is written to the audit log.
- Recalculate one job or all jobs.
- Issue and revoke player tokens.

**Map basemap.** Blocked on a decision: see open question 1.

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
4. **Server and admin.** Ingest, scoring, auth, review queue, audit log, recalculation.
5. **Public map.** Per-job pages with the projection and the approved basemap.

**Fixtures.** One fixture per bullet below.

*Spec boundaries*
- 105.0 / 125.0 mph in ATS, and 100.0 / 150.0 mph in ETS2, each exactly on the limit.
- A 1-second speed spike vs. sustained overspeed.
- Overspeed while on a ferry.
- 3000 vs. 3000.1 mi.
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

*Red-team cases*
- Pressing Esc repeatedly during overspeed.
- The revert-then-restore exploit.
- Accepting a job or quick job, which must not be read as a load.
- Delivery ordering.
- A delivery while the client is closed.
- A truck swap.
- Pausing in a menu after a load.

## 11. Open questions

1. **Map: stop condition triggered.** No source licenses ETS2/ATS map data for public display.
   - The best tooling is truckermudgeon/maps (GPL-3.0). It generates vector tiles from *your* game files. Its hosted tiles have CORS restricted to its own site and grant no license, so they must not be hotlinked or proxied.
   - The generated tiles are SCS game-derived data. SCS publishes no fan-content or map-data licence, and I could not read the EULA.
   - **Option (a):** self-generate tiles and publish them, accepting the risk, ideally after asking SCS for written permission.
   - **Option (b)** (recommended now): a public-domain Natural Earth basemap with the routes projected onto real geography, adding the game road layer only if SCS approves. Which do you want?
2. **Refuel.** The SDK has no refuel event, so "detected paid refuel" can only be a heuristic (the fuel rises while stationary at a plausible rate). A slow fake refuel while parked would pass. Is that acceptable?
3. **Captures.** Can you run the capture protocol in both games on this PC once stage 1 exists? The save-load thresholds cannot be finalized without it.
4. **Cancelled job brought back by a load.** Resume it (the cancel is reverted; recommended) or stop tracking it like a delivered job?
5. **Delivered while Invalid, then a mid-job save is loaded.** I recommend keeping your "stop tracking" rule, since a delivery is final, and letting admins review. Or should an Invalid delivery be reopenable?
6. **Review policy defaults.** Confirm that gaps and late starts are **held**, meaning no points until an admin approves, rather than flagged.
7. **Multiplayer (TruckersMP / Convoy).** Should these jobs be tracked? Loads are impossible in multiplayer, so load detection would be off when `multiplayer.time.offset ≠ 0`.
8. **Manual submission for patches below 1.36.** Add a portal form where every submission goes to review, or is that handled elsewhere?
9. **Profiles.** The SDK has no profile ID, so switching profiles is indistinguishable from loading a save. Is one timeline per player per game acceptable?
10. **Bonus cargo.** Can a flat bonus and a multiplier stack? Is the date window checked at delivery time on the server clock?
11. **Hosting and license.** Where will the server run (domain, TLS)? What license, if any, should PlasmaTrack itself use? For GitHub, create an empty private repo and send me the URL, or install `gh`.
