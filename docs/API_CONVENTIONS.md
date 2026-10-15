# Bot-facing API: conventions (v1)

**Audience:** the Discord bot (separate repo) and whoever implements the stage-4 server. This
file and its three companions are the contract:
- `docs/PUBLIC_API.md`: player features;
- `docs/ADMIN_API.md`: moderator features;
- `docs/WEBHOOKS.md`: server → bot events.

The client's upload API is separate (`docs/UPLOAD_API.md`).

**Status:** v1 draft, 2026-10-14. The server is not built yet (stage 4). The bot may build
against this now. Every change before the server ships is announced to the bot session and noted
in "Changes" at the end of this file. Requirements are answered from the bot's
`docs/CONTRACT_REQUIREMENTS.md`; the `§n` numbers below refer to it.

## 1. Base URL and versioning (§1.1)

- Every path is under `/api/v1/`.
- **Non-breaking changes** may appear in v1 at any time: new endpoints, new optional fields,
  new enum values, new webhook types.
  - **The bot must ignore unknown fields.**
  - **Unknown enum values must be treated as "other".**
- **A breaking change** means a new prefix (`/api/v2/`). v1 keeps working for at least 90 days
  after v2 ships. The change is announced in "Changes" below.

## 2. Authentication (§1.2, §1.3)

Two service tokens, both sent as `Authorization: Bearer <token>`:

| Token | Can call | Where it lives |
|---|---|---|
| **read token** | `GET /api/v1/public/...` only | Worker secret |
| **bot token** | everything: public reads, player self-service (`/api/v1/players/...`) and admin (`/api/v1/admin/...`) | Worker secret |

A leaked read token cannot change anything.

**Acting moderator.** Every `/api/v1/admin/...` call carries `X-PlasmaTrack-Actor: <Discord
user ID>` (a decimal snowflake string).
- **The server authorizes it.** The Discord ID must belong to a user with `role = admin` who is
  not disabled. Otherwise the call returns `403 forbidden`. The bot's own guild/role check is a
  second layer, not the authority.
- The actor's Discord ID is written into the audit log with every write.
- **Admins** are added in two ways:
  - **Bootstrap:** the Discord IDs in the Worker variable `BOOTSTRAP_ADMINS` are admins from
    their first call (docs/SERVER.md).
  - **Later:** `POST /api/v1/admin/admins` by an existing admin (ADMIN_API §12).

**Player self-service** (`/api/v1/players/{discord_id}/...`, bot token): the bot vouches that the
Discord interaction came from `{discord_id}`. Discord signs interactions, so the bot can prove it
to itself; the server trusts the bot token for this. These calls carry no actor header.

## 3. Writes: reason and idempotency (§1.4, §1.5)

**Reason.**
- **Where:** every admin write (`POST`, `PUT`, `PATCH`, `DELETE` under `/api/v1/admin/`) takes
  a JSON body field `"reason"`.
- **Length:** 3–1000 characters after trimming. Discord modals allow 4000; the bot should cap
  its modal at 1000.
- **Missing or out of range:** `422 validation_failed` with `details.fields.reason`.
- **Stored:** in the audit log with the action.
- **Self-service writes** (link, token rotation) take no reason; the audit log records "self".

**Idempotency.**
- **Header:** every write (admin and self-service) carries `Idempotency-Key: <string>`, 1–128
  printable ASCII. The bot derives it from the interaction ID.
- **Memory:** the server remembers each key for **24 hours**, per token.
- **Same key, same body:** the original response is returned again, same status. Nothing is
  re-applied.
- **Same key, different body:** `409 idempotency_mismatch`.
- **Missing header on a write:** `400 validation_failed`.

## 4. Errors (§1.6)

Every non-2xx response has this body:

```json
{
  "error": {
    "code": "invalid_transition",
    "message": "A discarded job cannot be approved.",
    "details": {"from": "discarded", "action": "approve"}
  }
}
```

`code` is stable; `message` is English for logs and may change.

| HTTP | `code` | When |
|---|---|---|
| 400 | `bad_request` | malformed JSON, bad query parameter |
| 401 | `unauthorized` | missing or unknown token |
| 403 | `forbidden` | read token on a write; actor not an admin; player disabled |
| 404 | `not_found` | unknown job, user, bonus, version… (`details.resource`) |
| 409 | `invalid_transition` | the action is not allowed from the current state (`details.from`, `details.action`) |
| 409 | `conflict` | the resource changed (`details.current_etag`); for example two moderators acting at once |
| 409 | `idempotency_mismatch` | the key was reused with a different body |
| 413 | `payload_too_large` | over the stated size limit (`details.max_bytes`) |
| 422 | `validation_failed` | per-field problems: `details.fields = {"reason": "required"}` |
| 429 | `rate_limited` | `Retry-After` header (seconds) and `details.retry_after_s` |
| 500 / 503 | `internal` / `unavailable` | retry with backoff; `Retry-After` on 503 |

**Rate limits:**

- **Commands:** each Discord user may run at most **5 bot commands per 20 seconds** (project
  owner, 2026-10-15). The bot enforces this, because it is the only side that sees commands; one
  command may make several API calls.
- **Server:** 600 requests per minute per token, and 60 writes per minute per actor. Over that
  returns `429`. *Not enforced yet*; the bot should still handle `429`.

**Latency:** every call answers within 10 s. Anything slower is an asynchronous operation
(ADMIN_API §7).

## 5. Lists and pagination (§1.7)

- **Request:** every list takes `?limit=` (default 20, maximum 100) and `?cursor=` (opaque).
- **Response:**
  ```json
  {"items": [ ... ], "next_cursor": "c3Rh...", "total": 412}
  ```
  - `next_cursor` is `null` on the last page.
  - `total` is present where it is cheap to compute (each list says so); otherwise it is absent.
- `next_cursor` is at most **64 printable ASCII characters** (base64url), so it fits in a
  Discord button `custom_id`.
- Cursors are valid for 24 hours. An expired cursor returns `400 bad_request`
  (`details.cursor = "expired"`).

## 6. Formats and units (§1.8–1.10)

- **Timestamps:** RFC 3339 UTC with `Z`, millisecond precision: `2026-10-13T21:25:00.070Z`.
- **Durations:** seconds, in fields ending `_s`.
- **Units are in the field name.** `_km` / `_mi` for distance, `_mph`, `_l` (litres).
- **Two distances, always both:**
  - `world_*`: physical 1:1 distance, the scored one;
  - `game_*`: in-game (odometer) distance, shown, not scored.

  The bot shows them as given ("12.3 mi driven (1:1), 146 mi in game"). It never converts,
  rounds or computes either.
- **Points:** already floored to the server's `points_decimals` (currently 1): `7.0`, `36.1`.
  Show as-is. JSON numbers, never strings.
- **Game:** `"ets2"` or `"ats"` on every job, leaderboard, bonus and stats object. No endpoint
  mixes the two games.
- **IDs:**
  - **Jobs:** `job_id` is 8 Crockford base32 characters. **In JSON it is always uppercase
    without a hyphen** (`7K2M9QXA`); show it as `7K2M-9QXA`. On input it is case-insensitive,
    the hyphen is optional, and `I`, `L` and `O` are read as `1`, `1` and `0`.
  - **Others:** opaque strings with a type prefix (`usr_…`, `bon_…`, `cfg_…`, `rel_…`, `rev_…`,
    `op_…`, `evt_…`, `tok_…`).
  - **Discord IDs** are decimal strings.

## 7. OpenAPI and examples (§1.11)

- **The Markdown files are the contract.**
- **`docs/openapi/bot-api.yaml`** (OpenAPI 3.1) and **`docs/examples/*.json`** are generated by
  `crates/pt-server/tests/contract.rs` from real responses of the server:
  - every endpoint and every webhook type;
  - deterministic, so they change only when the server's output does;
  - the test fails when they are stale.

  Response schemas are inferred from those examples, so where the Markdown says a field can be
  `null`, trust the Markdown.

## 8. Decisions taken in this contract

These answer the bot's open questions "(Q)". **The project owner confirmed all of them on
2026-10-14.** Any later change will be announced.

| # | Question | Default chosen |
|---|---|---|
| §1.2 | One token or two | Two: read token and bot token |
| §1.3 | Does the server authorize the actor | Yes: the actor's Discord ID must be an active admin |
| §2.12 | How users are linked | Both: self-service `/link`, and moderator-issued |
| §2.13 | TruckersMP account ID | Exposed: shown on the player's own public profile and on their TruckersMP jobs. It is *reported by the player's client, not verified*. It is never used to authenticate, and two moderator flags cover sharing (ADMIN_API §3) |
| §2.14 | Tokens | One active token per user. A new token revokes the old one. Players may rotate their own. The plaintext is returned once |
| §3.20 | Leaderboard periods | All-time, calendar month and ISO week (Monday start), in **UTC** |
| §3.20 | Singleplayer Division | Ranked on the main board as usual, plus a separate Singleplayer Division board (`?division=singleplayer`) |
| §4.27 | Is void reversible | Yes, with `restore` (reason required), which returns the job to its outcome before the void |
| §4.25 | Large traces | A time-limited signed download URL, never inline |
| §4.32 | Revocation date | The server computes `effective_at = announced_at + 21 days`; cancellable until then |
| §5.39 | Revocation recipients | One event per revocation, recipients in batches of at most 1000 (`part` / `parts`) |

## Changes

- 2026-10-14: first draft.
- 2026-10-14: job IDs in JSON are uppercase without a hyphen; `next_cursor` is at most 64
  characters; webhook bodies are at most 256 KiB; moderator-issued tokens may be collected
  later (ADMIN_API §10); new review reasons `fast_average` and `fast_average_game`
  (ADMIN_API §2).
- 2026-10-14: the project owner confirmed every decision in §8. The server runs on Cloudflare
  Workers (PLAN decision 15); the contract is unchanged by that.
- 2026-10-14: `distance_by_scale_class` (PUBLIC_API §3) carries `world_mi` and `game_mi` per
  class as well (requested by the bot).
- 2026-10-15: `docs/openapi/bot-api.yaml` and `docs/examples/*.json` are published (§7). The
  example display name is "Example Driver".
- 2026-10-15: the server is built (stage 4). Deployment details:
  - admins are bootstrapped with the Worker variable `BOOTSTRAP_ADMINS` (§2);
  - webhook URL and secrets are Worker secrets (WEBHOOKS §1);
  - trace downloads are streamed from storage by the Worker (ADMIN_API §6);
  - the two route endpoints answer `501 not_implemented` until stage 5 (map projection);
  - rate limits (§4) are not enforced yet;
  - a second, separate **test** deployment runs the same code with its own data and tokens.
- 2026-10-15: command rate limit of 5 per Discord user per 20 seconds, enforced by the bot (§4).
- 2026-10-15: route maps are live (stage 5). Both route endpoints answer `200` with
  `application/geo+json` (PUBLIC_API §4, ADMIN_API §5). New segment kind `ferry_or_train`, for
  a crossing the game did not name.
