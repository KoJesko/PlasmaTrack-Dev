# Bot-facing API: players and public reads (v1)

**Audience:** the Discord bot and the stage-4 server. Conventions (auth, errors, pagination,
units, IDs) are in `docs/API_CONVENTIONS.md`; read it first. `§n` refers to the bot's
`docs/CONTRACT_REQUIREMENTS.md`.

There are two parts:
- **player self-service** (`/api/v1/players/...`): bot token, on behalf of the Discord user who
  ran the command;
- **public reads** (`/api/v1/public/...`): read token or bot token.

## 1. Player self-service: linking and tokens (§2)

A Discord user is linked by being given a player token. The first token creates the PlasmaTrack
user for that Discord ID. Moderators can also issue tokens (ADMIN_API §10).

### `POST /api/v1/players/{discord_id}/token`: link, or rotate the token

Issues a new token. It creates the user if this Discord ID has none, and revokes any previous
token (one active token per user).

```json
{"display_name": "Example Driver"}
```

`display_name` is used only when the user is created: 2–32 characters, the Discord display name
by default. Requires `Idempotency-Key`.

**`201`:**
```json
{
  "user": {"user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver", "created": true},
  "token": {
    "token_id": "tok_4hq9x2",
    "token": "pt_live_3uJ8sX0mQ2vR7kLp9wZc4bNf6tYh1aEd",
    "created_at": "2026-10-14T09:12:03.120Z",
    "revoked_token_id": null
  }
}
```

- **`token.token` is the only time the plaintext exists in any response.** The server stores
  only its SHA-256.
- **The bot** shows it once, ephemerally, to that user, and never logs or stores it.
- **The player** pastes it into PlasmaTrack Setup.
- **A replay with the same `Idempotency-Key`** returns `200` with `token.token = null` and
  `details.replayed = true`, never the plaintext again.

Errors:
- `403 forbidden` if the user is disabled;
- `429` at more than 5 tokens per user per day.

### `GET /api/v1/players/{discord_id}/token`: active token metadata (§2.15)

```json
{"token_id": "tok_4hq9x2", "created_at": "2026-10-14T09:12:03.120Z", "last_used_at": "2026-10-14T20:01:44.000Z"}
```

`404 not_found` if the user is not linked or has no active token. The token value is never
returned.

### `DELETE /api/v1/players/{discord_id}/token`: revoke own token

Requires `Idempotency-Key`. Returns `200 {"revoked_token_id": "tok_4hq9x2"}`.

The client then gets `401` on upload and keeps its data locally until a new token is set.

### `PATCH /api/v1/players/{discord_id}`: change display name

```json
{"display_name": "Example Driver"}
```

Returns `200` with the player object (§2 below). Requires `Idempotency-Key`. Display names are
not unique.

## 2. Player profile and link status (§2.16, §3.21, §2.13)

### `GET /api/v1/public/players/{discord_id}`

```json
{
  "linked": true,
  "user_id": "usr_8f3k2m",
  "discord_user_id": "123456789012345678",
  "display_name": "Example Driver",
  "singleplayer_division": false,
  "disabled": false,
  "truckersmp_accounts": [
    {"account_id": 1234567, "profile_url": "https://truckersmp.com/user/1234567",
     "first_seen_at": "2026-10-13T20:21:12.047Z", "last_seen_at": "2026-10-13T21:28:19.000Z", "job_count": 3}
  ],
  "stats": {
    "ets2": {"points": 7.2, "job_count": 2, "world_km": 11.72, "world_mi": 7.28, "game_km": 126.7, "game_mi": 78.73, "pending_review_count": 1},
    "ats":  {"points": 39.7, "job_count": 2, "world_km": 64.02, "world_mi": 39.78, "game_km": 1144.0, "game_mi": 710.84, "pending_review_count": 0}
  }
}
```

- **Not linked:** `{"linked": false, "discord_user_id": "…"}` with status `200`, not 404.
- **`truckersmp_accounts`:** the account IDs the player's client reported while connected to a
  TruckersMP server.
  - **Reported, not verified:** a modified client could send any ID. Show them as
    "TruckersMP account(s) seen", never as proof of identity.
  - **ID 0** (unknown) is never listed.
- **`stats`:** count `valid` and `approved` jobs only. `pending_review_count` is shown
  separately. Both games are always present (zeros if unused). Each game's object also has:
  - **`outcomes`:** the number of finished jobs per outcome, all seven always present:
    `{"valid": 2, "approved": 1, "pending_review": 1, "invalid": 0, "rejected": 0, "discarded": 0, "voided": 0}`;
  - **`distance_by_scale_class`:** the totals of the jobs' `distance_by_scale_class` (§3),
    counting `valid` and `approved` jobs, in the same shape.

### `GET /api/v1/public/{game}/players/{discord_id}/stats`

The same `stats` object for one game, plus `"game": "ets2"`.

## 3. Jobs (§3.17, §3.18)

Only **finished** jobs are listed: delivered, or ended by a moderator. Jobs still in progress,
cancelled jobs, and suspended jobs that were never delivered are not.

### `GET /api/v1/public/{game}/players/{discord_id}/jobs`

Most recent delivery first. Paginated, with `total`. Filter `?outcome=` takes one or more
comma-separated outcomes. Each item is a **job summary**:

```json
{
  "job_id": "7K2M9QXA",
  "game": "ets2",
  "origin": "tracked",
  "outcome": "valid",
  "points": 7.0,
  "held_points": null,
  "world_km": 11.38, "world_mi": 7.07,
  "game_km": 125.7, "game_mi": 78.1,
  "cargo": {"id": "med_vaccine", "name": "Vaccines"},
  "source": {"city_id": "dover", "city_name": "Dover", "company_id": "euroacres", "company_name": "EuroAcres"},
  "destination": {"city_id": "calais", "city_name": "Calais", "company_id": "lkwlog", "company_name": "LKW Log"},
  "market": "cargo_market",
  "delivered_at": "2026-10-13T21:25:03.512Z",
  "reported_delivered_at": "2026-10-13T21:25:00.070Z",
  "multiplayer": {"kind": "truckersmp", "truckersmp": {"server_id": 4, "account_id": 1234567}},
  "fuel_used_l": 41.2, "damage_pct": 0.4, "income": 6767, "currency": "EUR"
}
```

**Fuel, damage and income** (public, like distance and points; owner, 2026-10-15). Each is
`null` when unknown:

- **`fuel_used_l`:** litres used on the job; the bot converts to gallons for ATS.
- **`damage_pct`:** **cargo** damage at delivery, 0–100. The tracker doesn't record truck wear, so
  a manual job's `damage` means cargo damage too.
- **`income`:** whole units of `currency` (`EUR` for ETS2, `USD` for ATS).
  - **Tracked jobs:** the game's revenue for the job.
  - **Manual jobs:** the submitted `profit`.
- **Moderator corrections** (ADMIN_API §14) of `fuel_used`, `damage` or `profit` show here.
- **Older jobs** show `null` until they are re-scored.

- **`outcome`** is one of:
  - `valid`, `approved`: count on the leaderboard;
  - `pending_review`: points held;
  - `invalid`, `rejected`, `discarded`, `voided`: 0 points.
- **`points`** is the counted total for `valid` and `approved`, `0` for the zero-point outcomes,
  and `null` while `pending_review`.
- **`held_points`** is what the job will count if approved. It is set only while
  `pending_review`.
- **`world_*`** is the scored 1:1 distance. **`game_*`** is the in-game distance of the same
  scored part. Both are 0 for `discarded`.
- **`multiplayer.kind`** is `singleplayer`, `truckersmp` or `convoy`. `truckersmp` is `null`
  unless the kind is `truckersmp`.
- **`delivered_at`** is the **server's** clock when the delivery reached it. Leaderboard periods
  and bonus windows use it, so a player's PC clock cannot move a job into another period.
  **`reported_delivered_at`** is the player's PC clock at the delivery, for display only. They
  differ by the upload delay, which can be hours if the client was offline.
- **`market`** is the game's `job.market`, for example `cargo_market`, `freight_market`,
  `quick_job` or `external_contracts` (World of Trucks). `null` for a manual job.
- **`origin`** is `tracked` (recorded by the tracker) or `manual` (submitted through the bot,
  §6). A manual job has:
  - `cargo.id`, `city_id` and `company_id` set to `null`, because only names were typed;
  - `market` set to `null`;
  - `multiplayer.kind` set to `unknown`.

### `GET /api/v1/public/players/{discord_id}/jobs`

The same list across **both games**, for a player's whole history: the same job summaries,
newest first, paginated with `total`, with the `?outcome=` filter. Each item's `game` says which
game it is from. This is the only endpoint that mixes the games.

### `GET /api/v1/public/jobs/{job_id}/series?max_points=1000`: graphs within a job

- **Who can see it:** everyone, but only for `valid` and `approved` jobs (owner: "the community
  helps catch cheating"). Every other outcome, and every manual job, gives `404`.
- **What it is:** the same shape as the admin series (ADMIN_API §5), with three differences:
  - no `reverted` windows;
  - no review `markers` (always `[]`);
  - only kept driving, as in the public route.
- **Each point has:**
  - speed, fuel and cargo damage ranges for its time bucket;
  - the distance from the job's start at the bucket's end:
    - `game_km`/`game_mi`: the odometer, all driving;
    - `world_km`/`world_mi`: the scored part at 1:1.

    These are the tracked figures. The job's final world distance comes from the game's delivery
    figure, so the two can differ slightly.

### `GET /api/v1/public/jobs/{job_id}`: job detail

The job summary plus:

```json
{
  "player": {"user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver"},
  "planned_distance_km": 62,
  "speed_penalty_applied": false,
  "bonus": {"bonus_id": "bon_2kd8", "label": "Medical week", "kind": "multiplier", "amount": 1.5},
  "distance_by_scale_class": {
    "city": {"world_km": 2.81, "world_mi": 1.75, "game_km": 8.43, "game_mi": 5.24},
    "road": {"world_km": 8.57, "world_mi": 5.33, "game_km": 117.27, "game_mi": 72.87},
    "one_to_one": {"world_km": 0.0, "world_mi": 0.0, "game_km": 0.0, "game_mi": 0.0},
    "nonstandard": {"world_km": 0.0, "world_mi": 0.0, "game_km": 0.0, "game_mi": 0.0},
    "unknown": {"world_km": 0.0, "world_mi": 0.0, "game_km": 0.0, "game_mi": 0.0}
  },
  "public_notes": [
    {"code": "speed_penalty", "text": "Speed penalty applied: 0.85× distance."}
  ],
  "custom_map": null
}
```

- **`custom_map`** is the manual job's custom map image (§6), as
  `{"url": "…", "expires_at": "…"}`: a signed link valid for an hour, so fetch it when showing
  the job. It is `null` for tracked jobs, when no map was submitted, and when a moderator
  approved the job with `hide_map` (ADMIN_API §7).

- **`bonus`** is `null` when no bonus applied. `kind` is `flat` (points added) or `multiplier`.
  Only the greatest matching bonus ever applies.
- **`distance_by_scale_class`** covers the driven, counted part. It always has all five classes. Each class
  carries both units.
  - **ETS2 `road`** includes UK roads (scale 15) as well as 19.
  - **ATS `road`** is scale 20.
  - **`city`** is scale 3 in both games.
- **`public_notes`** are player-safe explanations. `text` is ready-made English; the bot may show
  it or its own text per `code`. The codes are:
  - `speed_penalty`: the 0.85× penalty applied;
  - `under_review`: a moderator will check this job. It never says why; the reasons are
    admin-only;
  - `invalid_overspeed`: sustained speed over the invalid limit;
  - `discarded_integrity`: the tracker's files or settings were not recognised (rule 0);
  - `discarded_negative_distance`;
  - `discarded_singleplayer_division`: a TruckersMP job by a Singleplayer Division player;
  - `rejected`: rejected by a moderator. The moderator's reason is admin-only;
  - `voided`: voided by a moderator.

  Unknown codes may appear (API_CONVENTIONS §1).
- **Never in the public view:** reverted parts, review reasons, moderator reasons, integrity
  details or the trace.

## 4. Route (§3.19)

### `GET /api/v1/public/jobs/{job_id}/route`

`application/geo+json`, already projected to WGS84 lon/lat.

```json
{
  "type": "FeatureCollection",
  "bbox": [1.17, 50.95, 1.86, 51.13],
  "properties": {"job_id": "7K2M9QXA", "game": "ets2", "vertex_count": 1843, "max_vertices": 5000,
                 "simplification_tolerance_world_m": 5.0, "includes_reverted": false},
  "features": [
    {"type": "Feature",
     "geometry": {"type": "LineString", "coordinates": [[1.3133, 51.1268], [1.3141, 51.1262]]},
     "properties": {"index": 0, "segment": "driving", "local_scale": 3, "scale_class": "city",
                    "start_at": "2026-10-13T21:05:46.994Z", "end_at": "2026-10-13T21:08:33.112Z",
                    "counted": false, "world_km": 0.92, "game_km": 2.76}},
    {"type": "Feature",
     "geometry": {"type": "LineString", "coordinates": [[1.1876, 51.0959], [1.8579, 50.9513]]},
     "properties": {"index": 7, "segment": "train", "local_scale": null, "scale_class": "unknown",
                    "start_at": "2026-10-13T21:20:16.000Z", "end_at": "2026-10-13T21:20:24.000Z",
                    "counted": false, "world_km": 0.0, "game_km": 0.0}}
  ]
}
```

- **Features are in driving order** (`index` 0, 1, 2…). A new feature starts at every ferry,
  train, teleport, pause, gap and **scale change**.
- **`segment`** is one of:
  - `driving`: the tracked path;
  - `ferry` / `train`: a crossing, drawn as a straight line between its ends;
    `ferry_or_train` when the game did not say which;
  - `teleport`: F7, quick travel or a developer teleport, a straight line, with no distance;
  - `gap`: data missing (game crashed, client offline), a straight line. Its `game_km` and
    `world_km` are the odometer difference across the gap, when there is one.

  The renderer should draw the non-driving kinds differently, for example dashed.
- **`counted`:** whether this stretch earned distance (loaded and hitched; any job driving for
  World of Trucks). The empty drive to the pickup is `false`.
- **`scale_class`:** `city`, `road`, `one_to_one`, `nonstandard` or `unknown`.
  - Suggested colours: city grey, road blue, one_to_one orange, nonstandard red, unknown dashed.
  - **The bot draws modded and 1:1 routes over the vanilla basemap anyway.** A misaligned route
    is itself the visible sign of a modded map.
- **`local_scale`:** the reported number on driving features; `null` on the other kinds.
- **Size:** at most **5000 vertices** in total. The server simplifies the driving geometry
  (Douglas–Peucker on the game's coordinates) and states the tolerance used, converted to 1:1
  metres at the game's main road scale. Typical routes are far smaller: a 15-minute city
  delivery is about 170 vertices.
- **Projection:** the game's own Lambert conformal conic parameters (`def/climate.sii`), with
  the ETS2 UK correction credited to truckermudgeon/maps. Routes therefore line up with
  basemap tiles generated by truckermudgeon/maps.
- **Reverted parts** (undone by save loads) are **never** in this public route. The admin route
  (ADMIN_API §5) includes them, marked.

## 5. Leaderboards (§3.20)

### `GET /api/v1/public/{game}/leaderboard`

| Query | Values | Default |
|---|---|---|
| `period` | `all`, `month`, `week` | `all` |
| `at` | for `month`: `2026-10`; for `week`: ISO week `2026-W42` | the current period |
| `division` | `all`, `singleplayer` | `all` |
| `sort` | `points`, `distance` (1:1), `game_distance`, `fuel`, `income`, `damage` (average cargo damage) | `points` |
| `order` | `asc`, `desc` | `desc`; `asc` for `damage`, so the most careful driver ranks first |
| `limit`, `cursor` | API_CONVENTIONS §5 | 20 |

```json
{
  "game": "ets2", "period": "month", "at": "2026-10", "division": "all", "sort": "points", "order": "desc",
  "currency": "EUR",
  "period_start": "2026-10-01T00:00:00.000Z", "period_end": "2026-11-01T00:00:00.000Z",
  "generated_at": "2026-10-14T09:15:00.000Z",
  "items": [
    {"rank": 1, "user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver",
     "points": 97.4, "job_count": 5, "world_km": 158.2, "world_mi": 98.3, "game_km": 2850.0, "game_mi": 1770.9,
     "fuel_used_l": 812.4, "income": 61250, "damage_pct_avg": 1.2}
  ],
  "next_cursor": null, "total": 1
}
```

- **`sort`** ranks by that column. `rank` and ties follow it.
  - Players with no damage data (`damage_pct_avg: null`) rank last on a `damage` board.
  - `income` is in `currency`.

- **Periods are in UTC.** A job counts in the period of its `delivered_at`. Weeks are ISO weeks,
  starting Monday 00:00 UTC.
- **What counts:** `valid` and `approved` jobs. Points held for review do not count until
  approved.
- **Order:** by `sort` (points descending by default). Ties go to whoever reached that total
  first, then by `user_id`. Tied players get the same `rank` (1, 1, 3…).

### `GET /api/v1/public/{game}/players/{discord_id}/history`: a player over time

Counted jobs (`valid`, `approved`) summed per bucket of `delivered_at`, in UTC, for graphs.

| Query | Values | Default |
|---|---|---|
| `bucket` | `day`, `week` (ISO, Monday), `month` | `day` |
| `from`, `to` | an RFC 3339 instant or a date (`2026-10-15`); each picks the bucket it falls in | the last 30 days, 12 weeks or 12 months up to now |

```json
{
  "game": "ets2", "discord_user_id": "123456789012345678", "bucket": "week", "currency": "EUR",
  "from": "2026-10-05T00:00:00.000Z", "to": "2026-10-19T00:00:00.000Z",
  "before": {"job_count": 0, "points": 0.0, "world_km": 0.0, "…": "…"},
  "items": [
    {"start": "2026-10-12T00:00:00.000Z", "end": "2026-10-19T00:00:00.000Z",
     "job_count": 2, "points": 7.2, "world_km": 11.71, "world_mi": 7.28, "game_km": 126.7, "game_mi": 78.73,
     "fuel_used_l": 0.71, "income": 6767, "damage_pct_avg": 0.0,
     "running": {"job_count": 2, "points": 7.2, "world_km": 11.71, "…": "…"}}
  ]
}
```

- **Every bucket in the range is listed**, empty ones with zeros, so charts get an even axis.
  At most 400 buckets (`400`).
- **`running`** is the all-time total up to the end of that bucket. **`before`** is the total
  before the first bucket.
- **`damage_pct_avg`** is the average cargo damage of the jobs that report it; `null` when none
  do.
- **`points`** are floored per bucket and per running total.
- **`division=singleplayer`** ranks only Singleplayer Division players. On the `all` board they
  are ranked like everyone else.
- **Excluded:** disabled users.
- **Freshness:** the board can lag new results by up to 60 s.

## 6. Manual job submissions (owner, 2026-10-15)

A player can submit a job by hand. The bot collects it, shows a preview, and submits when the
player confirms. These calls use the **bot token**, on behalf of the player who ran the
command, and are writes (`Idempotency-Key`). A Discord user with no PlasmaTrack account yet gets
one, as with `/link`. A disabled player gets `403`.

### `POST /api/v1/players/{discord_id}/evidence`: upload one image

- **Body:** the image's raw bytes, not JSON, as **AVIF** (`image/avif`, preferred), WebP, PNG
  or JPEG.
  - The server checks the file's first bytes, not the `Content-Type` header. An AVIF is an
    ISO-BMFF file whose `ftyp` box names `avif` (or `avis`) as its major or a compatible brand.
  - Downloads (`/dl/evidence/…`) are served with the detected type.
- **Size:** at most **5 MB** per image (`413`).
- **Answer:** `201`:

  ```json
  {"evidence_id": "evd_0a1b2c3d4e", "content_type": "image/webp", "size_bytes": 182340,
   "sha256": "…", "expires_at": "2026-10-16T09:00:00.000Z"}
  ```

  An image that no submission uses by `expires_at` (24 hours) is deleted.
- **Limit:** at most 30 unused images per player at a time (`429`).

### `POST /api/v1/players/{discord_id}/manual-jobs`: submit

```json
{
  "game": "ets2", "unit": "km", "fuel_unit": "l",
  "start_city": "Calais", "start_company": "Tradeaux", "end_city": "London", "end_company": "Posped",
  "cargo_name": "Apples", "distance": 380, "fuel_used": 120.5, "damage": 2, "profit": 15000,
  "started_at": "2026-10-14T07:00:00Z", "ended_at": "2026-10-14T08:20:00Z",
  "truck": "Scania", "truck_model": "S 730", "mod_names": ["Example Map Mod"],
  "map_scale": 19,
  "uk": {"distance": 76, "entry_city": "Folkestone", "end_city": "London", "route_evidence": "evd_…"},
  "adjusted_scales": [
    {"entry_city": "Paris", "exit_city": "Paris", "distance": 6, "scale": 3, "evidence": "evd_…"},
    {"entry_city": "…", "exit_city": "…", "distance": 40, "scale": 1, "evidence": "evd_…"}
  ],
  "evidence": ["evd_…"],
  "custom_map": "evd_…"
}
```

- **Required:** `game`, `unit` (`mi` or `km`), `fuel_unit` (`l`, `us_gal` or `uk_gal`), the
  four places, `cargo_name`, `distance`, `fuel_used`, `damage`, `profit`, `started_at`,
  `ended_at`, `truck` and `evidence`.
- **Units:**
  - `unit` applies to every distance in the body, `fuel_unit` to `fuel_used`;
  - `distance` is what the game logged, in game distance;
  - `damage` is a percentage from 0 to 100;
  - `profit` is in the game's currency.
- **Optional fields:**
  - **`truck_model`, `mod_names`** (at most 100 names).
  - **`map_scale`:** when left out, ATS 20 and ETS2 19.
  - **`uk`** (ETS2 only): the stretch driven in the UK, counted at scale 15. All four fields or
    none. Its `distance` is part of `distance`.
  - **`adjusted_scales`:** 0 to **5** sections driven at another scale (owner, 2026-10-15).
    - Each section needs all five fields.
    - Their distances, plus the UK's, are part of `distance` and cannot add up to more.
  - **City sections need a minimum.** A section at **city scale (3)** must be at least
    **1 mile of real distance, which is 3 mi (4.828032 km) in game**. Shorter ones are
    rejected (`422`), not flagged for review. Moderators' edits follow the same rule.
  - **`custom_map`:** one image.
- **Images:**
  - `evidence` holds 1 to 5 images;
  - at most one each for `custom_map`, `uk.route_evidence` and each section's `evidence`, so at
    most 12 per job;
  - every image must have been uploaded for this player and not used by another job.
- **Validation:** `422 validation_failed` names every bad field, for example
  `{"damage": "a number from 0 to 100", "uk.distance": "…", "evidence[0]": "…"}`.
  - A section's fields are named by position: `adjusted_scales[2].distance`.
  - A city section that is too short gives
    `adjusted_scales[i].distance: "a city-scale section counts only from 3 mi in game (1 mi real)"`.
  - The older single `adjusted_scale` object is refused, with a pointer to `adjusted_scales`.
- **A player never picks a bonus.** `bonus_id` is refused here; a moderator can set it.
- **Answer:** `201` with the job detail (§3).
  - The job's `origin` is `manual`. It is always `pending_review`, with reason
    `manual_submission`, and its points held.
  - `job.needs_review` is sent, with `"origin": "manual"`.

**How a manual job is scored** (the server's rule, not the bot's):

- **World distance:** (distance − UK − all sections) ÷ `map_scale`, plus UK ÷ 15, plus each
  section ÷ its own `scale`. The bot never computes this.
  - Tracked jobs also count their city streets at scale 3. So the same game distance scores
    somewhat less when submitted by hand.
- **Points:** world miles × the usual points per mile, floored. No speed penalty, and no bonus
  unless a moderator sets one.
- **`distance_by_scale_class`:** each part's class comes from its scale (19, 20 and 15 are
  `road`, 1 is `one_to_one`, others `nonstandard`).
- **`delivered_at`** is the submission time, so a job cannot be dated into a closed period.
  `reported_delivered_at` is the submitted `ended_at`.
- **Route, series and trace** do not exist for a manual job (`404`).

### `GET /api/v1/players/{discord_id}/jobs/{job_id}/evidence`: the submitter's images

Only for the player who submitted the job (`404` otherwise):

```json
{"job_id": "1RQXYEB5", "items": [
  {"evidence_id": "evd_…", "role": "main", "section": null, "content_type": "image/webp", "size_bytes": 182340,
   "url": "https://…/dl/evidence/evd_…?exp=…&sig=…", "expires_at": "…"}
]}
```

- **`role`:** `main`, `custom_map`, `uk_route` or `adjusted_scale`.
- **`section`:** for `adjusted_scale` images, which section they show, numbered from 0 as
  submitted. The admin detail's `submission` keeps that order, even after moderators edit the
  sections. `null` for every other role.
- **`url`:** a signed link valid for **one hour**; fetching it needs no token.
- **Who sees images:**
  - moderators, in the admin job detail (ADMIN_API §4);
  - the submitter, through this call;
  - everyone else, only the `custom_map` (§3), and only unless it was hidden.

## 7. Downloading PlasmaTrack (owner, 2026-10-15)

### `GET /api/v1/public/client?platform=windows`: the newest release

Read token or bot token.

```json
{"platform": "windows", "release_id": "rel_0_2_0_windows", "version": "0.2.0",
 "published_at": "2026-10-15T10:24:00.000Z",
 "notes": "PlasmaTrack now updates itself: new versions install automatically once ETS2 and ATS are closed.",
 "installer": {"file_name": "PlasmaTrack-Setup-0.2.0.exe", "bytes": 103079567,
               "url": "https://plasmatrack-server.jansherremway.workers.dev/dl/setup/windows"}}
```

- **Which release:** the newest one offered to clients.
- **`404`** before any release is offered.
- **`installer`** is `null` for a release published without a setup program.

**`installer.url` stays the same across releases.** `GET /dl/setup/windows`, or just
`/dl/setup`, needs no token. It redirects (`302`) to the newest setup program, so the bot
can post it once and it never goes stale.

**Installed clients update themselves** (docs/UPDATES.md). A player only needs the setup:
- the first time;
- to move from 0.1.x, which has no updater, to 0.2.0.

**What clients ask:** `GET /api/v1/client/update?platform=windows&version=0.2.0`, without a
token. It returns `{"update": null}`, or the newer release's signed manifest. The bot does not
need it.
