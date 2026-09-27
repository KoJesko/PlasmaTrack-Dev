# Plugin ↔ client wire protocol (v1)

The plugin (`plugin/`) streams raw SDK data to the PlasmaTrack client over TCP on
IPv4 loopback. The **client listens** on `127.0.0.1:<port>`, and the **plugin connects out** from
its sender thread. The plugin always uses the literal `127.0.0.1`, never `localhost`, and
reconnects with backoff (250 ms, doubling, capped at 5 s).

The port comes from `plasmatrack.ini` next to the plugin module (default `30455`). The client
writes this file into every Steam library it finds, because Flatpak Steam cannot see the host
home directory.

## Framing

Both directions use the same frame format: a `u32` little-endian byte length, followed by that
many bytes of UTF-8 JSON (one object).

## Envelope (plugin → client)

Every message except `hello` and `snapshot` carries these fields:

| field | type | meaning |
|---|---|---|
| `v` | int | protocol version (1) |
| `kind` | string | see below |
| `sess` | string | 32 hex chars, random per `scs_telemetry_init` (a new session after every game start or `sdk reload`) |
| `seq` | u64 | strictly increasing within a session, starting at 1. A gap means the game thread dropped messages because its ring was full |
| `wall_ms` | i64 | host wall clock, Unix ms (for correlation only) |
| `frame` | u64 | game-thread frame counter (absent on messages created by the sender thread) |
| `sim_us` | u64 | `paused_simulation_time` of the latest `frame_start`, µs (scssdk_telemetry_event.h:132-136). It stops while physics is paused (absent on messages created by the sender thread) |

**Durable** kinds are journaled to disk until acknowledged: `init`, `config`, `gameplay`,
`paused`, `started`, `timer_restart`, `integrity`, `modules`, `shutdown`. `sample` and
`heartbeat` are memory-only.

## Kinds

| kind | extra fields |
|---|---|
| `hello` | `sess`, `plugin_version`, `last_seq`, `sample_rate_hz`, `replay_buffer_s`, `capture`, `ring_dropped`, `journal_dropped`, `log_dropped`. Sent first on every connection. Has no `seq`. |
| `snapshot` | `paused`, `configs` (array holding the latest `config` message per id), `last_sample` (a `sample` message or null). Sent second on every connection. Has no `seq`. |
| `init` | `api_version` (always "1.01"), `game_id` ("eut2" or "ats"), `game_name`, `game_version` {`major`, `minor`} (the telemetry game version, not the patch level), `plugin_version`, `platform`, `sample_rate_hz`, `capture`, `channels` {name: registered}, `events_failed` (comma list) |
| `integrity` | `plugin_path`, `plugin_sha256` (SHA-256 of the plugin file, for rule 0) |
| `modules` | `modules`: sorted base names of every module loaded in the game process. Re-sent when the list changes (checked every 10 s). The client matches TruckersMP names from `[multiplayer]`. |
| `config` | `id` (e.g. `job`, `truck`, `trailer.0`), `attrs` (array of attributes). An **empty** `attrs` means "none", e.g. no job (common/scssdk_telemetry_common_configs.h:125) |
| `gameplay` | `id` (e.g. `job.delivered`; unknown ids are forwarded too), `attrs` |
| `paused` / `started` | none (scssdk_telemetry_event.h:38-54) |
| `timer_restart` | `simulation_us`, `render_us`. The frame had `SCS_TELEMETRY_FRAME_START_FLAG_timer_restart` (scssdk_telemetry_event.h:91-97) |
| `sample` | `simulation_us`, `render_us`, `ch` {channel name: value or `null` = unavailable}, `stale` [names that had a value but no callback this frame] |
| `heartbeat` | `paused`, `ring_dropped`. Sent after 1 s with no other message from the game thread |
| `shutdown` | none. `scs_telemetry_shutdown` was called |

**Attribute** objects have the form `{"name": .., "index": null | n, "type": .., "value": ..}`.
- `type` is one of `bool s32 u32 u64 s64 float double fvector dvector euler fplacement dplacement string`.
- Vectors are rendered as `{"x","y","z"}` and euler angles as `{"heading","pitch","roll"}` (unit range, per scssdk_value.h:105-141).
- Placements are rendered as `{"position", "orientation"}`.
- Values keep SDK units: m/s, km, litres, in-game minutes, and world X east / Y up / Z south (scssdk_value.h:86-87).

**Sample channels** (all registered with `each_frame | no_value`):

| Channel | Type |
|---|---|
| `truck.speed` | float |
| `truck.odometer` | float |
| `truck.fuel.amount` | float |
| `truck.world.placement` | dplacement |
| `truck.local.velocity.linear` | fvector |
| `game.time` | u32 |
| `local.scale` | float |
| `multiplayer.time.offset` | s32 |
| `job.cargo.damage` | float |
| `truck.navigation.distance` | float |
| `truck.navigation.time` | float |
| `truck.engine.enabled` | bool |
| `truck.brake.parking` | bool |

**Sampling.** Samples are sent at `sample_rate_hz` of paused-simulation time. An extra sample is
also forced right after any config, gameplay, paused/started or timer_restart event. In capture
mode a sample is sent every frame. No samples are sent while paused.

## Client → plugin

The client sends one kind: `{"kind":"ack","sess":"<sess>","seq":N}`.
- It means that everything of that session up to and including `N` is **persisted** on the
  client side, so the plugin can drop it from its journal.
- Acks may cover old sessions that the plugin replays from disk.

## Replay and ordering

On every connection the plugin sends messages in this order:
1. `hello` and `snapshot`;
2. the unacknowledged journal entries of **earlier sessions** (their own `sess` values);
3. this session's unacknowledged durable messages and the buffered samples (the last
   `replay_buffer_s` wall seconds), merged by `seq`;
4. live messages.

The client must de-duplicate by `(sess, seq)` and order by `seq` within a session.
