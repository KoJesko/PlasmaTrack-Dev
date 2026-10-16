# Bot-facing API: moderators (v1)

**Audience:** the Discord bot and the stage-4 server. Read `docs/API_CONVENTIONS.md` first.

Every call here:
- uses the **bot token**;
- carries `X-PlasmaTrack-Actor: <Discord user ID>` of an active admin (API_CONVENTIONS §2);
- on writes, also carries `Idempotency-Key` and a `"reason"` (API_CONVENTIONS §3).

`§n` refers to the bot's `docs/CONTRACT_REQUIREMENTS.md`; `PLAN §n` to `docs/PLAN.md`.

## 1. Review queue (§4.22)

### `GET /api/v1/admin/review-queue`

| Query | Values | Default |
|---|---|---|
| `game` | `ets2`, `ats` | both |
| `outcome` | `pending_review`, `invalid` (comma-separated) | both |
| `reason` | a review reason code (§2) | any |
| `limit`, `cursor` | | 20 |

Oldest first, with `total`. Each item:

```json
{
  "job_id": "7K2M9QXA", "game": "ets2", "outcome": "pending_review",
  "player": {"user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver"},
  "held_points": 90.3, "world_km": 145.33, "game_km": 2512.0,
  "cargo": {"id": "med_vaccine", "name": "Vaccines"},
  "source": {"city_id": "honningsvag", "city_name": "Honningsvåg"},
  "destination": {"city_id": "bergen", "city_name": "Bergen"},
  "delivered_at": "2026-10-13T21:03:43.006Z", "age_s": 43920,
  "reasons": [{"code": "late_start", "text": "Tracking started after the job was accepted.", "details": {}}]
}
```

### `GET /api/v1/admin/review-queue/count`

`?game=` is optional.

```json
{"pending_review": 4, "invalid": 1, "by_reason": {"late_start": 2, "distance_mismatch": 1, "long_job": 1, "fuel_jump": 0}}
```

## 2. Reason codes

**Review reasons.** Any number per job; each puts the job in `pending_review`.

| `code` | Meaning | `details` |
|---|---|---|
| `long_job` | Rule 3: loaded world distance over the threshold (150 km) | `world_km`, `threshold_world_km` |
| `fast_average` | The whole job averaged over 5 1:1 km per real (unpaused) minute, i.e. 300 km/h | `world_km`, `driving_min`, `world_km_per_min` |
| `fast_average_game` | The same, timed by the game's clock: in-game minutes turned back into real minutes at the map scale (singleplayer) or 6 per minute (TruckersMP) | `world_km`, `game_clock_min`, `world_km_per_min` |
| `negative_fuel` | Rule 4: the tank gained more fuel in a run than its refuels explain (fuel used below −tolerance) | `run`, `used_l` |
| `fuel_jump` | Rule 4: fuel rose outside a detected refuel | `at`, `litres` |
| `gap` | Data missing mid-job (crash, client offline) | `count` |
| `late_start` | Tracking began after the job was accepted | |
| `cargo_switch` | Same route, different cargo mid-job (a save edit) | `from_cargo`, `to_cargo` |
| `distance_mismatch` | The game's delivery distance disagrees with the trace | `delivered_km`, `tracked_km`, `tolerance_km` |
| `no_delivered_distance` | The delivery carried no distance | |
| `time_warp` | The game ran faster than real time: the `warp` console command. Any 30 s of real driving with game time over 1.15× real time (honest play stays within 1.05) | `max_factor`, `gained_s` (game seconds gained) |
| `high_income` | The delivery paid more than `review_over_income_per_game_mi` (400) per mile of the game's delivery distance, or more than `review_over_income` (500,000) in all, in the game's currency. Every job, however short | `income`, `game_mi`, `per_game_mi`, `limit_per_game_mi`, `limit`, `currency` |
| `trace_check` | Sample rate, sequence or `hello` inconsistency (PLAN §6a) | `checks[]` |
| `odometer_jump`, `impossible_delivery_time` | Optional heuristics, off by default | rule specific |
| `manual_submission` | Submitted by hand through the bot (PUBLIC_API §6): every manual job has it | |

**Discard reasons.** These give `outcome = discarded`, and no moderator action can approve it.

| `code` | Meaning | `details` |
|---|---|---|
| `integrity` | Rule 0: config or install files match no published version | `config_matched`, `manifest_result` |
| `negative_distance` | Rule 1 | |
| `singleplayer_division_truckersmp` | A TruckersMP job by a Singleplayer Division player | |

**Invalid:** `outcome = invalid`. Its reason is `overspeed_invalid` (rule 2), with `peak_mph`,
`over_invalid_s` and `invalid_over_mph`. A moderator may approve it, for example after falling
through the map.

## 3. TruckersMP account flags (PLAN §8, decision 24)

### `GET /api/v1/admin/truckersmp/flags`

Paginated.

```json
{"items": [
  {"kind": "player_multiple_accounts", "user_id": "usr_8f3k2m", "discord_user_id": "1234…", "account_ids": [1234567, 7654321]},
  {"kind": "account_multiple_players", "account_id": 1234567, "users": [{"user_id": "usr_8f3k2m", "discord_user_id": "1234…"}, {"user_id": "usr_x2", "discord_user_id": "9876…"}]}
], "next_cursor": null}
```

These are information for moderators. They never change a job by themselves.

## 4. Admin job detail (§4.23)

### `GET /api/v1/admin/jobs/{job_id}`

This is the public job detail (PUBLIC_API §3), with `reasons` as in §1, plus:

```json
{
  "etag": "\"j7K2M9QXA-v5\"",
  "state": "delivered",
  "start_kind": "tracked",
  "profile_key": "a1b2c3d4e5f6",
  "plugin_sessions": ["13a7da3ef251e81e32ac4c9b3e53d29c"],
  "config_version": {"version_id": "cfg_3", "integrity_hash": "9f2c…", "current": true},
  "bonus_set": [{"bonus_id": "bon_2kd8", "label": "Medical week", "applied": true}],
  "multiplayer": {"kind": "truckersmp", "truckersmp": {"server_id": 4, "account_id": 1234567}},
  "distances": {
    "delivered_game_km": 239.0,
    "tracked_whole_game_km": 238.87, "tracked_counted_game_km": 125.58, "tracked_uncounted_game_km": 113.30,
    "counted_game_km": 125.70, "world_km": 11.384, "world_mi": 7.074, "accepted_world_mi": 7.074
  },
  "speed": {"peak_mph": 70.8, "over_penalty_s": 0.0, "over_invalid_s": 0.0, "penalty_over_mph": 100.0, "invalid_over_mph": 150.0},
  "fuel": {"used_l": 41.2, "refuels": [{"start_at": "…", "end_at": "…", "litres": 210.4}]},
  "rules": [
    {"rule": "integrity", "triggered": false, "details": {}},
    {"rule": "overspeed_penalty", "triggered": false, "details": {"over_penalty_s": 0.0}}
  ],
  "integrity": {
    "config_hash": "9f2c…", "config_matched_version": "cfg_3", "config_diff": [],
    "manifest_result": "ok", "manifest": [{"role": "plugin", "path": "…/plasmatrack.dll", "sha256": "572f…", "matched_release": "rel_0_1_0"}],
    "trace_checks": [{"check": "sample_rate", "ok": true, "details": {"declared_hz": 20, "measured_hz": 19.8}}]
  },
  "segments": [
    {"status": "kept", "from": "2026-10-13T21:05:46.994Z", "to": "2026-10-13T21:25:00.070Z", "sess": "13a7…", "seq_from": 182230, "seq_to": 217400},
    {"status": "reverted", "from": "…", "to": "…", "sess": "…", "seq_from": 0, "seq_to": 0, "by_load_at": "…"}
  ],
  "timeline": [
    {"at": "2026-10-13T21:05:46.994Z", "event": "started", "details": {"start": "tracked"}},
    {"at": "2026-10-13T21:14:53.186Z", "event": "cargo_loaded", "details": {}},
    {"at": "2026-10-13T21:20:24.962Z", "event": "teleport", "details": {"world_m": 6566.95, "ferry_or_train": true}},
    {"at": "2026-10-13T21:25:00.070Z", "event": "delivered", "details": {}}
  ],
  "audit": [ "…audit entries for this job, §13…" ]
}
```

- **`segments[].status`** is one of:
  - `kept`: counts;
  - `reverted`: undone by a save load;
  - `restored`: undone, then brought back by a later load;
  - `gap`: data missing.
- **`timeline[].event`** is one of `started`, `load_truncate`, `load_restore`, `load_gap`,
  `session_continue`, `teleport`, `suspended`, `resumed`, `cargo_loaded`, `cargo_switch`,
  `truck_change`, `delivered`, `cancelled`, `cancellation_reverted`,
  `invalid_delivery_reopened`, `stopped_after_delivery` or `duplicate_delivery`.
- **`etag`** changes on every change to the job. Send it as `expected_etag` on a decision to fail
  with `409 conflict` if another moderator acted first.

**Stats sheet, edits and manual jobs.** Every job's detail also has these fields:

```json
{
  "origin": "manual",
  "sheet": {"distance_km": 380.0, "distance_mi": 236.12, "map_scale": 19, "fuel_used_l": 120.5, "damage_pct": 2,
            "profit": 15000, "started_at": "…", "ended_at": "…", "truck": "Scania", "truck_model": "S 730",
            "mods": ["Example Map Mod"], "cargo_name": "Apples", "source_city": "Calais", "source_company": "Tradeaux",
            "destination_city": "London", "destination_company": "Posped",
            "uk": {"distance_km": 76.0, "distance_mi": 47.22, "entry_city": "Folkestone", "end_city": "London", "evidence_id": "evd_…"}},
  "overrides": {"distance_km": 384.0, "distance_mi": 238.6},
  "submission": {"unit": "km", "fuel_unit": "l", "map_scale_defaulted": true, "sheet": {"…": "as submitted"}},
  "evidence": [{"evidence_id": "evd_…", "role": "main", "content_type": "image/webp", "size_bytes": 182340, "url": "…", "expires_at": "…"}],
  "hide_map": false,
  "custom_map": {"url": "…", "expires_at": "…"}
}
```

- **`sheet`:** the job's effective values (canonical units: km with miles beside, litres, RFC 3339).
  - **Tracked job:** what the recording says, with any edits laid over it. Fields the recording
    cannot know are `null`: damage, profit, truck, map scale, UK and adjusted sections.
  - **Manual job:** what was submitted, with edits.
- **`overrides`:** only the fields a moderator changed (§14); `{}` when none.
- **`submission`:** for a manual job, the submitted values and the units the player used;
  `null` for tracked jobs.
- **`evidence`:** the images, each with a signed link valid for one hour. `[]` for tracked jobs.
- **`custom_map`:** as in the public detail, except moderators always see it, even with
  `hide_map` on.
- **Manual jobs:**
  - `speed` and `integrity` are `null`;
  - `rules`, `segments` and `timeline` are empty;
  - series, route and trace answer `404`.

## 5. Speed and fuel series; admin route (§4.24, §4.26)

### `GET /api/v1/admin/jobs/{job_id}/series?max_points=1000`

`max_points` defaults to 1000; the maximum is 4000. The kept timeline is split into at most
`max_points` time buckets. Each point carries the bucket's min and max, so peaks survive
downsampling.

```json
{
  "job_id": "7K2M9QXA", "game": "ets2", "bucket_s": 1.15,
  "penalty_over_mph": 100.0, "invalid_over_mph": 150.0,
  "points": [
    {"t": "2026-10-13T21:05:47.000Z", "speed_mph_min": 0.0, "speed_mph_max": 3.1, "fuel_l_min": 612.4, "fuel_l_max": 612.5, "reverted": false}
  ],
  "windows": [
    {"kind": "pause", "from": "…", "to": "…"},
    {"kind": "train", "from": "2026-10-13T21:20:16.000Z", "to": "2026-10-13T21:20:24.000Z"},
    {"kind": "reverted", "from": "…", "to": "…"},
    {"kind": "refuel", "from": "…", "to": "…", "litres": 210.4}
  ],
  "markers": [{"kind": "fuel_jump", "at": "…", "litres": 35.0}]
}
```

- **Speed** is world mph, the same as the speedometer.
- **Each point also has** (owner, 2026-10-15, for graphs):
  - `damage_pct_min` and `damage_pct_max`: cargo damage 0–100, or `null` where not reported;
  - `game_km`/`game_mi`: the odometer distance from the job's start, all driving;
  - `world_km`/`world_mi`: the scored part at 1:1 from the start.

  The distances are taken at the bucket's end. Older jobs show `null` damage until their data is
  replayed.
- **Public version:** `GET /api/v1/public/jobs/{job_id}/series`, for `valid` and `approved` jobs
  only, without reverted windows and review markers (PUBLIC_API §3).
- **Pauses, ferries and trains** are excluded from overspeed. Their windows are listed so the
  chart can shade them.
- **Size:** about 120 bytes per point, so 4000 points stay under 500 KB.

### `GET /api/v1/admin/jobs/{job_id}/route`

The same schema as PUBLIC_API §4, with these differences:
- `properties.includes_reverted` is `true`;
- every feature also has `"status": "kept" | "reverted" | "restored"`;
- reverted stretches are separate features, so the renderer can draw them, for example faded.

## 6. Full trace (§4.25)

### `GET /api/v1/admin/jobs/{job_id}/trace`

Returns a time-limited signed URL. The Worker streams the file from storage.

```json
{"url": "https://pt.example.org/dl/trace/7K2M9QXA?exp=1792000000&sig=…",
 "expires_at": "2026-10-14T09:30:00.000Z", "size_bytes": 1846089,
 "format": "jsonl+gzip", "frames": 15800}
```

- **The file:** gzip JSONL holding every raw frame of the job's plugin sessions, from accept to
  end, in plugin order (`docs/WIRE.md`).
  - Each line has an extra field, `"pt_status": "kept" | "reverted" | "restored" | "outside"`.
  - Module lists are redacted, as in uploads.
- **The URL** is valid for 15 minutes and needs no other auth. Each request is audited as a read
  by the actor.
- **Showing it:** if `size_bytes` ≤ 10 MiB, the bot may download it and attach it. Otherwise it
  posts the URL ephemerally to the moderator.

## 7. Decisions (§4.27)

`POST /api/v1/admin/jobs/{job_id}/{action}`, where `action` is `approve`, `reject`, `void` or
`restore`:

```json
{"reason": "Fell through the map at Calais, confirmed on the route.", "expected_etag": "\"j7K2M9QXA-v5\""}
```

`expected_etag` is optional.

| Action | Allowed from | Result |
|---|---|---|
| `approve` | `pending_review`, `invalid`, `rejected` | `approved`: the held (or computed) points count |
| `reject` | `pending_review`, `valid`, `approved`, `invalid` | `rejected`: 0 points |
| `void` | any except `discarded`, `voided` | `voided`: 0 points; the job stays visible as voided |
| `restore` | `voided` | the outcome before the void |

- **Discarded jobs are never approvable.** Only a recalculation (§8) can change them, for example
  after a missing config version is published.
- **Any other combination** returns `409 invalid_transition`.
- **`reject` always needs its reason.** The bot sends a standard one when the moderator types
  none.
- **`approve` takes two optional fields, applied in the same step:**

  ```json
  {"reason": "Screenshots match.", "expected_etag": "…", "hide_map": true,
   "corrections": {"unit": "km", "distance": 384, "uk": {"distance": 80}}}
  ```

  - `hide_map`: keep a manual job's custom map out of the public detail.
  - `corrections`: any fields of §14, with `unit` and `fuel_unit` as given.
    - They are stored as overrides, the job is re-scored, and then it is approved. The answer's
      `points` are the corrected points.
    - The correction is audited as `job.edit`, the approval as `job.approve`.
    - Only the approval's `job.scored` is sent.

  Either field on any other action is `422`.

**`200`:**
```json
{"job_id": "7K2M9QXA", "outcome": "approved", "points": 90.3, "held_points": null, "etag": "\"j7K2M9QXA-v6\""}
```

The server then emits `job.scored` (WEBHOOKS §4).

## 8. Recalculation (§4.29)

**Moderator decisions stick.** `approved`, `rejected` and `voided` keep their outcome through a
recalculation; their points are recomputed (for example a new bonus applies to an approved job).
Every other outcome is re-derived from the raw trace with the **current** config version and
bonuses.

### `POST /api/v1/admin/jobs/{job_id}/recalculate`

Synchronous. Body `{"reason": "…"}`.

```json
{"job_id": "7K2M9QXA",
 "before": {"outcome": "pending_review", "points": null, "held_points": 90.3},
 "after":  {"outcome": "pending_review", "points": null, "held_points": 90.3},
 "etag": "\"j7K2M9QXA-v7\""}
```

### `POST /api/v1/admin/recalculations`

Asynchronous.

```json
{"reason": "Bonus for medical cargo added", "game": "ets2", "dry_run": true}
```

`game` is optional; leaving it out means both games.

- **Dry run:** `200 {"affected_jobs": 1243, "game": "ets2"}`. The bot can show "this will
  rescore 1243 jobs" before the reason modal.
- **Real run:** `202 {"operation_id": "op_9xk2", "affected_jobs": 1243}`.

### `GET /api/v1/admin/operations/{operation_id}`

```json
{"operation_id": "op_9xk2", "kind": "recalculation", "status": "running",
 "progress": {"done": 410, "total": 1243}, "started_at": "…", "finished_at": null,
 "summary": null}
```

- **`status`:** `queued`, `running`, `succeeded` or `failed`.
- **When finished:** `summary` lists the counts of changed outcomes, `{"pending_review→valid": 3, …}`.
- **Webhook:** completion also emits `operation.completed` (WEBHOOKS §4).
- **One at a time:** only one recalculation runs at once. Another returns
  `409 conflict` with the running `operation_id`.

## 9. Bonus cargo (§4.28)

`/api/v1/admin/bonuses`: `GET` (list; `?game=`, `?status=active|scheduled|ended|archived`),
`POST` (create), `GET /{bonus_id}`, `PATCH /{bonus_id}`, `DELETE /{bonus_id}`.

```json
{"game": "ets2", "label": "Medical week", "match": "med_*", "match_kind": "glob",
 "kind": "multiplier", "amount": 1.5,
 "active_from": "2026-10-19T00:00:00.000Z", "active_to": "2026-10-26T00:00:00.000Z",
 "reason": "October event"}
```

- **`match`** is matched against the cargo ID (`cargo.id`, for example `med_vaccine`).
  - **`match_kind: exact`:** equal, ignoring case.
  - **`match_kind: glob`:** `*` matches any run of characters and `?` one character.
- **`kind`:**
  - `flat`: `amount` points added;
  - `multiplier`: the accepted distance's points × `amount` (> 1).
- **The window** is in UTC instants, half-open `[active_from, active_to)`. It is checked against
  the job's **`delivered_at`**: the server's clock when the delivery reached it (PUBLIC_API §3).
- **Only the greatest matching bonus applies** (PLAN §6, rule 7).
- **`DELETE` archives the bonus** and needs a reason. Jobs keep the bonus they were scored with.
  An archived bonus no longer applies to new jobs or recalculations, and is listed under
  `?status=archived`.
- Responses are the bonus object with `bonus_id`, `status`, `created_at` and `updated_at`.

## 10. Player tokens and accounts (§2.14, §2.15, §4.33)

### `GET /api/v1/admin/players/{discord_id}`

The public profile (PUBLIC_API §2) plus `role`, `created_at`, the active token's metadata
(`token_id`, `created_at`, `last_used_at`; never the value) and the player's TruckersMP flags.

### `POST /api/v1/admin/players/{discord_id}/token`

`{"reason": "…", "display_name": "…"}`. This is the same as self-service
(PUBLIC_API §1): it creates the user if needed and returns the plaintext once.

**Showing the token:** the bot shows it only to the player, never in a moderator channel. It can
put it in an ephemeral message the player opens with a button, or a DM. The moderator who issued
it does not see it.

**Collecting it later is fine.** The bot may make this call when the player clicks "Get your
token", minutes or days after the moderator's command. It sends the issuing moderator as
`X-PlasmaTrack-Actor` and their stored reason.
- The audit entry records that moderator, the reason, and the time of the call. That time is the
  moment the token came into existence.
- If that moderator is no longer an admin by then, the call returns `403 forbidden`, and the
  player has to ask again.

### `DELETE /api/v1/admin/players/{discord_id}/token`

`{"reason": "…"}` revokes the active token.

### `PATCH /api/v1/admin/players/{discord_id}`

`{"singleplayer_division": true, "disabled": false, "display_name": "…", "reason": "…"}`. Every
field is optional; `reason` is required.

- **`singleplayer_division`** applies to jobs delivered **after** the change. A recalculation
  (§8) applies it to earlier jobs.
- **`disabled`** blocks uploads (`401`) and hides the player from leaderboards. Jobs are kept.

## 11. Config versions, releases and revocations (§4.30–4.32)

### `POST /api/v1/admin/config-versions`

```json
{"toml": "…full plasmatrack.toml text…", "make_current": true, "reason": "Tolerance change"}
```

- **Size:** `toml` is at most **256 KiB**.
- **`201`:**
  `{"version_id": "cfg_4", "integrity_hash": "…", "current": true, "created_at": "…"}`.
- **Invalid TOML or config:** `422 validation_failed`, with the validator's messages in
  `details.errors[]`.
- **`[local]` is ignored**, because it is not part of the integrity hash.

`GET /api/v1/admin/config-versions` lists them (newest first, with the `current` flag).
`POST /api/v1/admin/config-versions/{version_id}/make-current` takes `{"reason"}`.

### `POST /api/v1/admin/releases`

```json
{"release_version": "0.1.0", "platform": "windows",
 "files": [{"role": "plugin", "path_pattern": "*/bin/win_x64/plugins/plasmatrack.dll", "sha256": "572f0d20…"},
           {"role": "client_binary", "path_pattern": "*/plasmatrack-clientw.exe", "sha256": "…"}],
 "reason": "Release 0.1.0"}
```

- **Size:** at most **1 MiB**.
- **`platform`:** `windows` or `linux`.
- **`role`:** `plugin`, `client_binary` or `config_file`.
- `GET /api/v1/admin/releases` lists them.

**Updates** (docs/UPDATES.md). `scripts/publish-release.mjs` adds these optional fields; the bot
does not need to send them.

| Field | Meaning |
|---|---|
| `update` | `{"manifest": "<text>", "signature": "<hex>"}`: the release's files, signed with the release key. |
| `installer` | File name of the setup program in the release bucket, such as `PlasmaTrack-Setup-0.2.0.exe`. |
| `notes` | What changed, for players: at most 2000 characters. |
| `offer` | Offer the update to clients now. Defaults to `true` when `update` is given. |

`422` when:
- the signature is not by a release key;
- the manifest is for another version or platform;
- a plugin or client hash of the release is missing from the update, or the update ships a
  program that is not one of the release's files;
- a file or the setup program is not in the release bucket, or its size differs.

Each release in responses gains:
- `update`: `{"files": 10, "bytes": 8536073}`, or `null`;
- `installer`;
- `notes`;
- `offered`.

### `POST /api/v1/admin/releases/{release_id}/offer`

`{"offered": false, "reason": "Crashes on start"}` stops offering a release's update; `true`
starts again.
- **Who gets what:** clients get the newest offered release that no revocation targets. A
  scheduled revocation counts as well.
- **`200`:** the release. `422` when offering a release that has no update.
- **Audit action:** `release.offer`.

### Revocations (PLAN decision 17)

`POST /api/v1/admin/revocations`:

```json
{"target": {"kind": "release", "id": "rel_0_1_0"}, "reason": "Plugin bug fixed in 0.1.1"}
```

- **`target.kind`:** `release` or `config_version`.
- **The server sets** `announced_at = now` and `effective_at = announced_at + 21 days`.
- **`201`:**
  `{"revocation_id": "rev_k2", "announced_at": "…", "effective_at": "…", "recipients_count": 37}`.
- **After it:** jobs captured with the target after `effective_at` are discarded (rule 0);
  earlier jobs are unaffected.
- **Webhook:** emits `version.revocation_scheduled` (WEBHOOKS §4).

The other revocation calls:
- `GET /api/v1/admin/revocations?status=scheduled|effective|cancelled` lists them.
- `POST /api/v1/admin/revocations/{revocation_id}/cancel` (`{"reason"}`) cancels one before
  `effective_at`. After that it returns `409 invalid_transition`.

## 12. Admins (API_CONVENTIONS §2)

- `GET /api/v1/admin/admins` lists the admins.
- `POST /api/v1/admin/admins` (`{"discord_user_id": "…", "reason": "…"}`) adds one. The user
  is created if needed.
- `DELETE /api/v1/admin/admins/{discord_id}` (`{"reason": "…"}`) removes one. Removing the
  last admin returns `409 invalid_transition`.

## 13. Audit log (§4.34)

### `GET /api/v1/admin/audit`

Filters: `?job_id=`, `?actor=<discord id>`, `?target_user=<discord id>`, `?action=`,
`?since=`, `?until=`. Newest first, paginated.

```json
{"items": [
  {"audit_id": "aud_77", "at": "2026-10-14T09:20:11.000Z",
   "actor": {"discord_user_id": "1234…", "display_name": "Example Driver"},
   "action": "job.approve", "target": {"kind": "job", "id": "7K2M9QXA"},
   "reason": "Fell through the map at Calais, confirmed on the route.",
   "before": {"outcome": "invalid"}, "after": {"outcome": "approved", "points": 90.3}}
], "next_cursor": null}
```

- **Every write** in this file is audited, as are player self-service writes (actor `self`) and
  trace downloads.
- **`action`** values are `<resource>.<verb>`, for example `job.approve`, `bonus.create`,
  `token.issue`, `player.update`, `config.publish`, `revocation.schedule`, `trace.download`.

## 14. Editing a job (owner, 2026-10-15)

### `PATCH /api/v1/admin/jobs/{job_id}`

Corrects any field of **any** job, tracked or manual:

```json
{"reason": "The delivery distance was cut off.", "expected_etag": "\"j7K2M9QXA-v5\"",
 "unit": "km", "fuel_unit": "l",
 "distance": 412, "cargo_name": "Corrected cargo", "damage": 4, "truck": "Peterbilt", "bonus_id": "bon_2kd8"}
```

- **Required:** `reason`. `expected_etag` is optional, as in §7.
- **Fields:** the same names as a manual submission (PUBLIC_API §6):
  - places, `cargo_name`, `truck`, `truck_model`, `mod_names`;
  - `distance`, `fuel_used`, `damage`, `profit`, `started_at`, `ended_at`;
  - `map_scale`, `uk`, `adjusted_scales`, plus `bonus_id` and `speed_penalty`.
- **Units and rules for fields:**
  - **Units:** `unit` is needed with any distance, `fuel_unit` with `fuel_used`.
  - **`uk`** may be partial (`{"uk": {"entry_city": "Dover"}}`), and `null` removes it.
  - **`adjusted_scales`** is edited by section number, counting from 0 in the current list
    (the admin detail's `sheet`):

    ```json
    {"adjusted_scales": {"0": {"distance": 12}, "2": null, "3": {"entry_city": "…", "exit_city": "…", "distance": 4, "scale": 1}}}
    ```

    - **Change a section:** give an object with any of its fields.
    - **Remove one:** `null`.
    - **Add one:** use the next number, here 3 for a list of three; it needs every field except
      the image.
    - **Replace the whole list:** send a list (no images).
    - **Remove them all:** `null`.
    - **Limits:** at most 5 sections, and the city-scale minimum (PUBLIC_API §6) applies.
  - **Images** of `uk` and the sections cannot be changed.
  - **Clearing:** `null` clears `damage`, `profit`, `fuel_used`, `truck`, `truck_model`,
    `mod_names` and `map_scale` (back to the game's default); `null` on `bonus_id` means no
    bonus. Other fields cannot be cleared.
  - **Restoring:** setting a field back to the recording's or the submission's value removes
    its override.
  - **Errors:** an unknown field is `422 validation_failed` with
    `details.fields.<name> = "unknown field"`. A body with nothing to change is `422` too.
- **Edits survive re-scoring.** They are stored as overrides and applied again by every
  recalculation, bonus change and replay.
- **What changes the points:**
  - **Names, fuel, damage, profit, times, truck and mods:** shown values only.
  - **`distance` alone** on a tracked job: the job's own city/road mix is kept, so world
    distance scales with it.
  - **`map_scale`, `uk` or `adjusted_scales`** on a tracked job: its world distance is worked
    out the manual way (PUBLIC_API §6).
  - **`bonus_id`:** replaces the automatic bonus choice. It must be a bonus of the job's game,
    in any state.
  - **`speed_penalty`** (owner, 2026-10-16): `true` adds the speed penalty (the distance counts at
    `rules.speed_penalty_factor`, 0.85), `false` removes it, and `null` lets the rules decide
    again. It works on tracked and manual jobs; players cannot set it. The admin detail's
    `sheet.speed_penalty` shows what the rules decided, and `speed_penalty_applied` the result.
  - **Otherwise the speed penalty** of a tracked job still applies.
- **The outcome does not change.** A moderator decided outcome stays.
- **Answer `200`:**

  ```json
  {"job_id": "7K2M9QXA", "outcome": "valid", "points": 7.3, "held_points": null,
   "overrides": {"cargo_name": "Corrected cargo", "damage_pct": 4.0}, "etag": "\"j7K2M9QXA-v6\""}
  ```

- **Webhooks:** `job.scored` with `by: {"kind": "moderator", …}` when the points changed.
- **Audit:** `job.edit`, with outcome, points and overrides before and after.

## 15. Backups (owner, 2026-10-15)

### `POST /api/v1/admin/backups`

Writes a backup of the server's database now, besides the automatic weekly one
(docs/SERVER.md). Body: `{"reason": "…"}`. Audited as `backup.create`.

**`201`:**

```json
{"key": "d1/plasmatrack-server/2026-10-15T20-00-00.000Z.sql", "size_bytes": 104845,
 "rows": {"jobs": 7, "users": 6, "…": 0}, "deleted": ["d1/plasmatrack-server/2026-10-04T03-47-00.000Z.sql"]}
```

Only the newest two backups are kept, so `deleted` lists what this one replaced.
