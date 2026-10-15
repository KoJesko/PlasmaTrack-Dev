-- Manual job submissions, moderator edits and evidence images (ADMIN_API §14,
-- PUBLIC_API §6).

-- tracked: built from the player's recording; manual: submitted through the bot.
ALTER TABLE jobs ADD COLUMN origin TEXT NOT NULL DEFAULT 'tracked';
-- Manual jobs: the submission in canonical units (km, litres, ms), as submitted.
ALTER TABLE jobs ADD COLUMN submission TEXT;
-- Moderator edits in effect, field → value (null clears a section). Applied
-- again every time the job is scored.
ALTER TABLE jobs ADD COLUMN overrides TEXT NOT NULL DEFAULT '{}';
-- A moderator approved the job without its custom map being public.
ALTER TABLE jobs ADD COLUMN hide_map INTEGER NOT NULL DEFAULT 0;
CREATE INDEX jobs_user_all ON jobs (user_id, delivered_at);

-- Images uploaded through the bot. Unattached ones are deleted after a day.
CREATE TABLE evidence (
    evidence_id  TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    job_id       TEXT,                -- set when a submission uses it
    role         TEXT,                -- main | custom_map | uk_route | adjusted_scale
    content_type TEXT NOT NULL,
    size_bytes   INTEGER NOT NULL,
    sha256       TEXT NOT NULL,
    r2_key       TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE INDEX evidence_job ON evidence (job_id);
CREATE INDEX evidence_user ON evidence (user_id, job_id, created_at);
