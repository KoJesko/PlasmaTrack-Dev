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
| Web | **Not in this repo.** The public site (MapLibre GL JS + pmtiles, self-generated tiles) lives in a separate repo and consumes this server's public JSON API | Your decision. This repo owns the API contract and route projection. |
| Admin | Admin API + signed webhooks consumed by your **Discord bot (separate repo)** | Your decision; this repo only owns the API contract. |
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
| Load to a no-job save or a different job's save | Current job → **Suspended**: not cancelled, not sent to review, no penalty. The other job is resumed (if suspended), tracked (if new, as a late start), or stopped (if already delivered). |
| Suspended job's fingerprint comes back | Resume, using the three "Load, same job" rows above. |
| Load brings back a **cancelled** job (decided) | Resume it; the cancellation is reverted. Exception: if that fingerprint was ever delivered, stop tracking. |
| Load into an **already delivered** job | **Stop tracking** until the fingerprint changes. The server also rejects a second delivery of the same fingerprint for the same player. **Exception (decided): a delivery that was Invalid is reopened**, because the invalidity may come from falling through the map. The Invalid delivery is marked `reverted_by_save_load`, the job resumes, and a later delivery replaces it. |
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
0. **Integrity** (added at your request; runs first; overrides the spec's "no other auto-invalidation"). The job is **discarded** (0 pts, reason and diff visible to admins, not approvable) if either check fails (§6a):
   - the uploaded config's integrity hash matches no server-published config version;
   - the uploaded install manifest matches no published release.
1. Negative kept-timeline distance → Discard.
2. Sustained speed over `invalid_over_mph` → Invalid.
3. Driven distance over 3000 mi → Pending review.
4. Fuel used below −tolerance → **manual review** (points held). Also send every mid-run fuel jump outside a detected refuel to manual review, with its timestamp.
5. Sustained speed over `penalty_over_mph` → accepted distance = 0.85 × driven.
6. 1 point per accepted mile.
7. Bonus cargo (decided): among all matching bonuses, **only the greatest applies**, meaning the one that yields the most points. It stacks with the 0.85× speed penalty: bonus applied to the accepted distance. Flat and multiplier bonuses never stack with each other. The date window is checked at delivery time on the server clock.
- Final points = floor(total).

**Evaluation**
- **Overspeed:** eligible samples exclude paused time and ferry/train transfer windows. The record keeps peak mph and time over each threshold.
- **Refuel (decided):** the SDK has no refuel event, so a refuel is a **heuristic** episode: the fuel rises while the truck is stationary, at a plausible rate (`[rules.refuel]`). Only litres are tracked. Fuel cost is never calculated.
- **Fuel across save loads (decided):** a fuel change across a load is most likely a save edit, and it is **ignored**. There's no flag and no rule 4, because fuel is only summed within tracked runs.
- **Manual review everywhere (decided): every "flag" means manual review**, with the job pending and its points held until an admin approves or rejects it via the Discord bot. `review_policy` values are `review` or `off`. Gap, late-start and trace checks default to `review`.
- **Other heuristics:** teleport, odometer jump and impossible delivery time exist only as manual-review triggers, **off by default**. Diagnostics (unexplained Δodometer, derived vs. reported speed, fuel against capacity) are always *stored* for admins but never trigger review.
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
- **The SDK has no profile identifier** (§3), so `profile_key` must come from outside the SDK. The client will read candidate sources, and the captures (§9) decide which is reliable:
  - the game's own log file;
  - which profile folder under the game's documents directory is being written.
  - Neither is confirmed yet. Under Proton these files live inside the Wine prefix, and the client will locate them.
- Until confirmed, the key is `unknown`, and profile switches fall back to load handling.

**Multiplayer is tracked** in every form: TruckersMP, other multiplayer mods, and Convoy.
- **Convoy** is detected from `multiplayer.time.offset ≠ 0` (common_channels.h:287-300).
- **TruckersMP** cannot be identified by the SDK. The plugin reports whether any module named in `[multiplayer] truckersmp_module_names` is loaded in the game process. That list stays empty until a capture confirms the names.
- **Singleplayer Division:** a user flag, set through the admin API. For these users, jobs detected as TruckersMP are **discarded**, with the reason shown. Convoy and other multiplayer remain allowed.
- Each job stores its multiplayer kind.

**Player notifications** (desktop notification from the client, via `notify-rust`, MIT/Apache):
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
| `jobs` | user, **game**, fingerprint, state, start_kind (tracked/late_start), config snapshot fields, truck_key, start/end odometer and fuel, refuelled_l, driven/accepted mi, points, peak mph, seconds over each threshold, delivered attributes, outcome (discarded / invalid / pending_review / valid / approved / rejected / voided), review_reasons[], multiplayer (none / convoy / truckersmp / other), profile_key, config_version |
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

## 8. Server APIs (the public site and the Discord bot live in separate repos)

This repo ships **no HTML**. The server exposes versioned JSON APIs, and two separate repos consume them:
- the **public site repo**: per-job map pages and leaderboards;
- the **Discord bot repo**: all admin actions.

The contracts are documented here as OpenAPI (`docs/PUBLIC_API.md`, `docs/ADMIN_API.md`), with contract tests.

**Public read API** (`/api/v1/public/...`, CORS limited to the site's origin, configured on the server):
- Per-game job list and leaderboard: `/ets2/...` and `/ats/...`. Games are never combined.
- Job detail: stats, outcome, rule results that are safe to show publicly.
- **Route as GeoJSON** in lon/lat, already projected, split into LineStrings at ferries, trains, pauses and gaps.
- Reverted material is never exposed publicly.

**Route projection** stays in this repo (`pt-core::projection`, server-side), so the site only draws GeoJSON.
- Game world X/Z (metres) → lon/lat. Implemented independently from the game's `def/climate.sii` parameters and standard Lambert conformal conic maths. No GPL code is copied; truckermudgeon is credited for the ETS2 UK correction.
- ATS: inverse LCC (lat₁ 33, lat₂ 45, lat₀ 39, lon₀ −96).
- ETS2: offset (−16660, −4150), a UK box scaled 0.75 around (15550, 2750), then LCC (37, 65, 50, 15).
- Unit tests use four known points (Paris, Crete, London, Las Vegas).

**Admin API for the Discord bot** (`/api/v1/admin/...`):
- review queue: every job pending manual review (with its reasons), plus invalid jobs;
- job detail with every rule result, the full trace (reverted parts marked), and speed/fuel series and route GeoJSON, so the bot or a site review page can chart and map them;
- approve, reject or void, each with a **required reason**;
- bonus cargo CRUD;
- recalculate one job or all jobs;
- issue and revoke player tokens;
- publish config versions and release manifests (rule 0).
- **Auth:** a bot service token plus the acting admin's Discord user ID on every call. That ID goes into the audit log.
- **Webhooks:** signed POSTs (`job.needs_review`, `job.scored`, `job.discarded`) to a URL the bot registers.
- **Gap and late-start jobs are held for admin approval** (`review_policy = "hold"`).

**Map basemap (decided; built and hosted in the site repo):**
- You generate ETS2 and ATS vector tiles (PMTiles) with truckermudgeon/maps (GPL-3.0) from your own game install. This is allowed for modding as long as nothing is sold, so **PlasmaTrack must stay non-commercial**.
- The site repo renders them with MapLibre GL JS + pmtiles (both BSD-3) and self-hosts them. No hotlinking.
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
4. **Server APIs.** Ingest, scoring, rule 0, auth, the public read API, the admin API and webhooks for the Discord bot, audit log, recalculation, and OpenAPI contract tests.
5. **Route projection and GeoJSON endpoint.** The pages themselves are built in the separate site repo.

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

1. **Map:** you generate the tiles yourself with truckermudgeon/maps. This is allowed for modding as long as nothing is sold, so PlasmaTrack stays non-commercial. The tiles are rendered in the separate site repo (§8).
2. **Refuel:** the heuristic is accepted. Fuel cost is never calculated. Fuel spikes across save loads are treated as save edits and ignored (§6).
3. **Captures:** you will run the capture protocol in both games (§9).
4. **Cancelled job brought back by a load:** resume it, unless that job was already delivered (§5).
5. **Review:** every "flag" means manual review, with points held until an admin approves. This applies to gap, late-start, trace checks and rule 4. All admin actions run through your Discord bot in a separate repo (§6, §8).
6. **Minimum patch:** 1.36. Earlier versions are unsupported. Manual submissions go to a website you'll provide later.
7. **Integrity:** rule 0 discards jobs whose config or install files match no published version. Trace checks send the job to manual review (§6a).
8. **Public site:** a separate repo, which doesn't exist yet. This repo exposes the JSON API and projected route GeoJSON only (§8).
9. **Delivered-but-Invalid job brought back by a load:** it is reopened, because falling through the map can cause it (§5).
10. **Notifications:** desktop notifications for an Invalid delivery and for the speed penalty activating, each delayed by a random 5–30 s at µs resolution (§6b).
11. **Multiplayer:** all of it is tracked. For Singleplayer Division users, TruckersMP jobs are discarded (§6b).
12. **Profiles:** separate timelines per profile (§6b).
13. **Bonuses:** only the greatest bonus applies, and it stacks with the 0.85× penalty (§6).
14. **License:** PolyForm Noncommercial 1.0.0, which allows reuse and modification but no commercial use (`LICENSE.md`, `NOTICE`).
15. **Hosting:** deferred.

## 12. Open questions (none block stage 1)

1. **Revocation.** Should the server be able to revoke an old release or config version, so that jobs captured with it are discarded too?
2. **Discord bot contract.**
   - Does the bot already exist with an API shape I should match? If not, I'll define `docs/ADMIN_API.md` and you build the bot against it.
   - Does the bot also issue player tokens, for example via a `/register` command?
3. **Notification channel.** I've assumed desktop notifications from the client. Should they also go out as a webhook event, so the bot can DM the player?
4. **DLC-gated map areas.** Shown or hidden on the public map? This is decided in the site repo.
5. **Licensor name.** `NOTICE` uses your git name, "Ari Kotler". Should it be a team or VTC name instead?
