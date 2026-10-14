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
  separately. Both games are always present (zeros if unused).

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
  "multiplayer": {"kind": "truckersmp", "truckersmp": {"server_id": 4, "account_id": 1234567}}
}
```

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
  `quick_job` or `external_contracts` (World of Trucks).

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
  ]
}
```

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
  - `teleport`: F7, quick travel or a developer teleport, a straight line, with no distance;
  - `gap`: data missing (game crashed, client offline), a straight line.

  The renderer should draw the non-driving kinds differently, for example dashed.
- **`counted`:** whether this stretch earned distance (loaded and hitched; any job driving for
  World of Trucks). The empty drive to the pickup is `false`.
- **`scale_class`:** `city`, `road`, `one_to_one`, `nonstandard` or `unknown`.
  - Suggested colours: city grey, road blue, one_to_one orange, nonstandard red, unknown dashed.
  - **The bot draws modded and 1:1 routes over the vanilla basemap anyway.** A misaligned route
    is itself the visible sign of a modded map.
- **`local_scale`:** the reported number, or `null` when not reported.
- **Size:** at most **5000 vertices** in total. The server simplifies the driving geometry
  (Douglas–Peucker in world metres) and states the tolerance used. Typical routes are far
  smaller.
- **Reverted parts** (undone by save loads) are **never** in this public route. The admin route
  (ADMIN_API §5) includes them, marked.

## 5. Leaderboards (§3.20)

### `GET /api/v1/public/{game}/leaderboard`

| Query | Values | Default |
|---|---|---|
| `period` | `all`, `month`, `week` | `all` |
| `at` | for `month`: `2026-10`; for `week`: ISO week `2026-W42` | the current period |
| `division` | `all`, `singleplayer` | `all` |
| `limit`, `cursor` | API_CONVENTIONS §5 | 20 |

```json
{
  "game": "ets2", "period": "month", "at": "2026-10", "division": "all",
  "period_start": "2026-10-01T00:00:00.000Z", "period_end": "2026-11-01T00:00:00.000Z",
  "generated_at": "2026-10-14T09:15:00.000Z",
  "items": [
    {"rank": 1, "user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver",
     "points": 97.4, "job_count": 5, "world_km": 158.2, "world_mi": 98.3, "game_km": 2850.0, "game_mi": 1770.9}
  ],
  "next_cursor": null, "total": 1
}
```

- **Periods are in UTC.** A job counts in the period of its `delivered_at`. Weeks are ISO weeks,
  starting Monday 00:00 UTC.
- **What counts:** `valid` and `approved` jobs. Points held for review do not count until
  approved.
- **Order:** points descending. Ties go to whoever reached that total first, then by `user_id`.
  Tied players get the same `rank` (1, 1, 3…).
- **`division=singleplayer`** ranks only Singleplayer Division players. On the `all` board they
  are ranked like everyone else.
- **Excluded:** disabled users.
- **Freshness:** the board can lag new results by up to 60 s.
