# Upload API (client → server), v1

This is the contract the stage-4 server implements. The Rust types are
`pt_core::upload::{UploadChunk, UploadAck, ManifestEntry}`, shared by client and server.

## `POST /api/v1/chunks`

**Headers**

| Header | Value |
|---|---|
| `Authorization` | `Bearer <player token>` |
| `Content-Type` | `application/json` |
| `Content-Encoding` | `gzip` |

**Body:** `UploadChunk`

| field | meaning |
|---|---|
| `protocol` | `1` |
| `client_version` | client version string |
| `sess` | plugin session id (32 hex) |
| `hello` | latest `hello` message of that session (docs/WIRE.md), or null |
| `seq_from`, `seq_to` | seq range covered by `frames` (inclusive) |
| `frames` | raw plugin messages of that session, in seq order. Seq gaps are plugin-side drops. `modules` messages are redacted: `modules` is replaced by `module_count`, `truckersmp_matches` and `redacted: true` |
| | plus **client notes**, which have no `seq`: `{"v":1,"kind":"profile","sess","wall_ms","profile_key","after_seq"}`. Each sits right after plugin frame `after_seq` (or before the next frame when that one was dropped or sent in an earlier chunk). The server keeps the array order |
| `config_hash` | `Config::integrity_hash()` of the client's effective config |
| `config` | `Config::integrity_canonical_json()`: everything except `[local]` |
| `manifest` | `[{role, path, sha256}]`. `role` is `client_binary`, `config_file` or `plugin` |

**Success: `200`** with body `UploadAck` `{"sess": "...", "accepted_through": N}`. It means
every frame of `sess` up to seq `N` is stored.

**Idempotency.** The server stores frames by `(player, sess, seq)` and ignores duplicates (a note by `(player, sess, after_seq, profile_key, wall_ms)`), so
the client may resend any chunk at any time.

**Any non-2xx response** (or a network error) means the client retries the same data after
`[sync] retry_interval_wall_s` (20 s), forever.

Rule 0 and every other scoring decision is made by the server after storing. An upload is
never rejected *because of* its content, so evidence is always kept. Only authentication and
malformed-request failures are rejected, and uploads from a client older than the server's
`MIN_CLIENT_VERSION` (Worker variable): `426 client_too_old`, details
`{client_version, min_client_version}`. Like any non-2xx it is retried, so the data waits on
the PC until the client has updated (`GET /api/v1/client/update` is not gated).
