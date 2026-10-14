-- PlasmaTrack server schema (D1 = SQLite). Applied with
-- `wrangler d1 migrations apply`, and by the native tests (pt-server).
-- Times are Unix milliseconds. Discord IDs are TEXT (snowflakes exceed 2^53).

CREATE TABLE users (
    user_id               TEXT PRIMARY KEY,
    discord_user_id       TEXT NOT NULL UNIQUE,
    display_name          TEXT NOT NULL,
    role                  TEXT NOT NULL DEFAULT 'player',   -- player | admin
    singleplayer_division INTEGER NOT NULL DEFAULT 0,
    -- When the flag last changed: it applies to jobs delivered after that.
    division_changed_at   INTEGER NOT NULL DEFAULT 0,
    disabled              INTEGER NOT NULL DEFAULT 0,
    created_at            INTEGER NOT NULL
);

CREATE TABLE tokens (
    token_id      TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL,
    token_sha256  TEXT NOT NULL UNIQUE,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER,
    revoked_at    INTEGER
);
CREATE INDEX tokens_user ON tokens (user_id, revoked_at);

-- One row per plugin session of a player.
CREATE TABLE plugin_sessions (
    user_id        TEXT NOT NULL,
    sess           TEXT NOT NULL,
    game           TEXT,                 -- ets2 | ats, once the init frame is seen
    first_wall_ms  INTEGER NOT NULL,     -- the player's clock: orders sessions
    received_at    INTEGER NOT NULL,
    hello          TEXT,
    client_version TEXT,
    config_hash    TEXT,
    config_json    TEXT,
    manifest       TEXT,
    stored_through INTEGER NOT NULL DEFAULT 0,   -- highest plugin seq stored
    chunk_count    INTEGER NOT NULL DEFAULT 0,
    notes_seen     TEXT NOT NULL DEFAULT '[]',   -- identities of stored client notes
    PRIMARY KEY (user_id, sess)
);
CREATE INDEX plugin_sessions_game ON plugin_sessions (user_id, game, first_wall_ms);

-- Raw frames live in R2; this indexes them. Only new frames are stored, so the
-- chunks of a session never overlap and replay in chunk_no order.
CREATE TABLE raw_chunks (
    user_id     TEXT NOT NULL,
    sess        TEXT NOT NULL,
    chunk_no    INTEGER NOT NULL,
    seq_from    INTEGER NOT NULL,
    seq_to      INTEGER NOT NULL,
    frames      INTEGER NOT NULL,
    bytes       INTEGER NOT NULL,
    r2_key      TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, sess, chunk_no)
);

CREATE TABLE jobs (
    job_id               TEXT PRIMARY KEY,
    user_id              TEXT NOT NULL,
    game                 TEXT NOT NULL,
    profile_key          TEXT NOT NULL,
    local_id             INTEGER NOT NULL,   -- pt-core's id in that profile timeline
    version              INTEGER NOT NULL,   -- job_version (webhooks) and the etag
    state                TEXT NOT NULL,      -- pt-core JobState
    start_kind           TEXT NOT NULL,
    outcome              TEXT NOT NULL,      -- valid | pending_review | invalid | discarded | approved | rejected | voided | incomplete
    auto_outcome         TEXT NOT NULL,      -- what the rules say
    pre_void_outcome     TEXT,
    points               REAL,
    held_points          REAL,
    computed_points      REAL NOT NULL DEFAULT 0,  -- what approval would count
    world_km             REAL NOT NULL DEFAULT 0,
    game_km              REAL NOT NULL DEFAULT 0,
    cargo_id             TEXT NOT NULL DEFAULT '',
    market               TEXT NOT NULL DEFAULT '',
    reasons              TEXT NOT NULL DEFAULT '[]',
    multiplayer          TEXT NOT NULL DEFAULT 'singleplayer',
    tmp_server_id        INTEGER,
    tmp_account_id       INTEGER,
    bonus_id             TEXT,
    config_version_id    TEXT,
    delivered_at         INTEGER,            -- server clock when the delivery arrived
    reported_delivered_at INTEGER,           -- the player's clock
    started_wall_ms      INTEGER NOT NULL DEFAULT 0,
    summary              TEXT NOT NULL,      -- JSON: names, distances, speed, bonus, notes
    detail               TEXT NOT NULL,      -- JSON: admin detail (rules, integrity, timeline…)
    updated_at           INTEGER NOT NULL,
    UNIQUE (user_id, game, profile_key, local_id)
);
CREATE INDEX jobs_user ON jobs (user_id, game, delivered_at);
CREATE INDEX jobs_outcome ON jobs (game, outcome, delivered_at);

CREATE TABLE truckersmp_accounts (
    user_id       TEXT NOT NULL,
    account_id    INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    PRIMARY KEY (user_id, account_id)
);
CREATE INDEX truckersmp_accounts_account ON truckersmp_accounts (account_id);

CREATE TABLE bonuses (
    bonus_id    TEXT PRIMARY KEY,
    game        TEXT NOT NULL,
    label       TEXT NOT NULL,
    match       TEXT NOT NULL,
    match_kind  TEXT NOT NULL,      -- exact | glob
    kind        TEXT NOT NULL,      -- flat | multiplier
    amount      REAL NOT NULL,
    active_from INTEGER NOT NULL,
    active_to   INTEGER NOT NULL,
    archived_at INTEGER,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE audit_log (
    audit_no     INTEGER PRIMARY KEY AUTOINCREMENT,
    at           INTEGER NOT NULL,
    actor        TEXT NOT NULL,     -- a Discord ID, or 'self' / 'system'
    action       TEXT NOT NULL,
    target_kind  TEXT NOT NULL,
    target_id    TEXT NOT NULL,
    target_user  TEXT,              -- Discord ID of the affected player
    reason       TEXT,
    before       TEXT,
    after        TEXT
);
CREATE INDEX audit_target ON audit_log (target_kind, target_id);

CREATE TABLE config_versions (
    version_id     TEXT PRIMARY KEY,
    version_no     INTEGER NOT NULL UNIQUE,
    toml           TEXT NOT NULL,
    integrity_hash TEXT NOT NULL,
    timeline_hash  TEXT NOT NULL,   -- hash of [timeline]: a change needs a full replay
    current        INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    created_by     TEXT NOT NULL
);

CREATE TABLE releases (
    release_id      TEXT PRIMARY KEY,
    release_version TEXT NOT NULL,
    platform        TEXT NOT NULL,
    files           TEXT NOT NULL,  -- JSON [{role, path_pattern, sha256}]
    created_at      INTEGER NOT NULL,
    created_by      TEXT NOT NULL,
    UNIQUE (release_version, platform)
);

CREATE TABLE revocations (
    revocation_id TEXT PRIMARY KEY,
    target_kind   TEXT NOT NULL,    -- release | config_version
    target_id     TEXT NOT NULL,
    announced_at  INTEGER NOT NULL,
    effective_at  INTEGER NOT NULL,
    cancelled_at  INTEGER,
    message       TEXT NOT NULL
);

CREATE TABLE idempotency (
    scope       TEXT NOT NULL,      -- which token
    key         TEXT NOT NULL,
    body_sha256 TEXT NOT NULL,
    status      INTEGER NOT NULL,
    response    TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (scope, key)
);

CREATE TABLE webhook_outbox (
    event_id     TEXT PRIMARY KEY,
    type         TEXT NOT NULL,
    body         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    delivered_at INTEGER,
    dropped_at   INTEGER,
    last_error   TEXT
);

CREATE TABLE operations (
    operation_id TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    status       TEXT NOT NULL,     -- queued | running | succeeded | failed
    game         TEXT,
    total        INTEGER NOT NULL,
    done         INTEGER NOT NULL DEFAULT 0,
    summary      TEXT NOT NULL DEFAULT '{}',
    requested_by TEXT NOT NULL,
    reason       TEXT NOT NULL,
    started_at   INTEGER NOT NULL,
    finished_at  INTEGER
);
