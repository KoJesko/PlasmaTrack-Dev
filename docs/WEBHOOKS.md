# Webhooks: server → Discord bot (v1)

**Audience:** the Discord bot and the stage-4 server. Read `docs/API_CONVENTIONS.md` first.
`§n` refers to the bot's `docs/CONTRACT_REQUIREMENTS.md`.

The server POSTs signed JSON events to one URL that the bot owns.

## 1. Registration (§5.38)

There is no API for this. Both values are **Worker secrets** of the server (docs/SERVER.md),
one set per deployment (production and test):

```sh
wrangler secret put WEBHOOK_URL       # https://plasmatrack-bot.<account>.workers.dev/webhooks/plasmatrack
wrangler secret put WEBHOOK_SECRETS   # the current secret; "new,old" during rotation (§2)
```

- **One URL receives every event type.**
- **The secret:** the project owner generates it (32 random bytes, base64url) and sets the same
  value in the server config and as a Worker secret.

## 2. Signature (§5.35)

Every request carries these headers:

| Header | Value |
|---|---|
| `Content-Type` | `application/json` |
| `X-PlasmaTrack-Event` | the event type, e.g. `job.scored` |
| `X-PlasmaTrack-Event-Id` | the same as `event_id` in the body |
| `X-PlasmaTrack-Signature` | `t=<unix seconds>,v1=<hex>`; during rotation `t=<unix seconds>,v1=<hex>,v1=<hex>` |

**What is signed:** the exact bytes `"{t}.{raw body}"`. That is the decimal timestamp, a full
stop, then the request body exactly as received, before any JSON parsing. The algorithm is
**HMAC-SHA256** with the secret's UTF-8 bytes as the key; `v1` is the lowercase hex digest.

**The bot verifies** like this:
1. Parse `t` and every `v1` value.
2. Reject the request if `|now − t| > 300` seconds.
3. Compute the HMAC over `"{t}.{raw body}"` and compare it in constant time with each `v1`.
   Accept the request if any of them matches.

```js
// Cloudflare Worker (Web Crypto)
const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret),
  {name: "HMAC", hash: "SHA-256"}, false, ["sign"]);
const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(`${t}.${rawBody}`));
const hex = [...new Uint8Array(mac)].map(b => b.toString(16).padStart(2, "0")).join("");
```

**Rotation:** set `WEBHOOK_SECRETS` to `new,old`. The server then signs every
request with each listed secret, one `v1` per secret. After that:
1. Switch the bot to the new secret.
2. Set `WEBHOOK_SECRETS` to the new secret alone.

## 3. Envelope and delivery (§5.36, §5.37)

```json
{
  "event_id": "evt_01JAB7C9X2",
  "type": "job.scored",
  "occurred_at": "2026-10-14T09:20:11.204Z",
  "payload_version": 1,
  "data": { ... }
}
```

- **Body size:** at most **256 KiB**. A 1000-recipient revocation batch is about 25 KB.
- **Success** is any `2xx` within 10 s. The bot should answer once the event is queued.
- **Retries:** on any other status, a timeout or a network error, the server retries after
  30 s, 2 min, 10 min, 30 min, 1 h, 2 h and 4 h, then every 8 h. An event older than **72 h**
  is dropped and logged on the server.
- **At least once:** an event may arrive more than once. Deduplicate on `event_id`.
- **No ordering guarantee.** Job events carry `job_version`, which increases with every change
  to that job. Ignore an event whose `job_version` is lower than one already handled for the
  same `job_id`.
- **`payload_version`** changes only for a breaking change to `data`. New optional fields can
  appear without a change.

## 4. Events (§5.39)

The `player` object is `{"user_id", "discord_user_id", "display_name"}`. Reason objects and
codes are as in ADMIN_API §2.

### `job.needs_review`

A job has entered the review queue: either `pending_review` or `invalid`.

```json
{"job_id": "7K2M9QXA", "game": "ets2", "job_version": 3,
 "player": {"user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver"},
 "outcome": "pending_review", "held_points": 90.3,
 "reasons": [{"code": "late_start", "text": "Tracking started after the job was accepted.", "details": {}}],
 "delivered_at": "2026-10-13T21:03:46.120Z"}
```

### `job.scored`

A job's outcome or points changed. It fires:
- on the first automatic `valid` result;
- after a moderator decision;
- when a recalculation changes the job.

The bot updates or closes the matching review-queue message.

```json
{"job_id": "7K2M9QXA", "game": "ets2", "job_version": 4,
 "player": {"user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver"},
 "outcome": "approved", "previous_outcome": "pending_review",
 "points": 90.3, "held_points": null,
 "by": {"kind": "moderator", "discord_user_id": "987654321098765432"}}
```

- **`by.kind`:**
  - `rules`: automatic scoring;
  - `moderator`: a decision, with the moderator's Discord ID;
  - `recalculation`: with `operation_id` when it came from a bulk run.
- **The moderator's reason is not included.** Read it from the audit log (ADMIN_API §13) if
  needed.

### `job.discarded`

```json
{"job_id": "4QH82KDM", "game": "ats", "job_version": 1,
 "player": {"user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678", "display_name": "Example Driver"},
 "reasons": ["integrity"]}
```

### `player.notification`

A DM for one player (PLAN decision 16).

```json
{"recipient": {"user_id": "usr_8f3k2m", "discord_user_id": "123456789012345678"},
 "kind": "speed_penalty_activated",
 "job_id": "7K2M9QXA", "game": "ets2",
 "happened_at": "2026-10-13T20:44:10.500Z",
 "title": "Speed penalty",
 "text": "You drove over 100 mph for too long. This job's distance now counts at 0.85×."}
```

- **`kind`:** `invalid_delivery` or `speed_penalty_activated`.
- **The server has already applied the random 5–30 s delay** (PLAN decision 16) before sending
  this event, so **the bot sends the DM immediately**. The desktop client's own delayed
  notification is separate and independent.
- **Timing:** the server only learns of the event when the client uploads the data containing
  it, so `happened_at` can be minutes earlier than delivery (or hours if the player was
  offline).
- **`title` and `text`** are player-safe English, ready to send. The bot may use its own wording
  per `kind` instead.
- **DMs closed:** the bot stores the notice for that Discord user (its own design, CONTRACT
  §6). The server is not told.

### `version.revocation_scheduled`

A release or config version will stop being accepted (PLAN decision 17). This tells the players
whose **latest** session used it.

```json
{"revocation_id": "rev_k2",
 "target": {"kind": "release", "id": "rel_0_1_0", "label": "PlasmaTrack 0.1.0 (Windows)"},
 "announced_at": "2026-10-14T09:00:00.000Z",
 "effective_at": "2026-11-04T09:00:00.000Z",
 "message": "PlasmaTrack 0.1.0 stops counting jobs on 4 November 2026 at 09:00 UTC. Run PlasmaTrack Setup to update.",
 "recipients": ["123456789012345678", "234567890123456789"],
 "part": 1, "parts": 1}
```

- **`effective_at`** is always `announced_at + 21 days`.
- **Recipients** come in batches of at most 1000 Discord IDs. Each batch is its own event, with
  its own `event_id`, `part` and `parts`. The bot fans the DMs out through its queue at Discord's
  rate limits.
- **`message`** is player-safe. The moderator's reason is admin-only.
- **Cancelled revocations** emit `version.revocation_cancelled`, with `revocation_id`, `target`
  and `recipients` in the same batches. The bot may DM "no longer needed", or ignore it.

### `operation.completed`

A bulk operation finished (ADMIN_API §8).

```json
{"operation_id": "op_9xk2", "kind": "recalculation", "status": "succeeded",
 "started_at": "…", "finished_at": "…",
 "summary": {"jobs": 1243, "changed": {"pending_review→valid": 3}},
 "requested_by": {"discord_user_id": "987654321098765432"}}
```
