-- Client updates (docs/UPDATES.md): a release can carry a signed update
-- manifest, whose files are in the release bucket, and the setup program.
-- Clients are offered the newest release with offered = 1 that is not revoked.
ALTER TABLE releases ADD COLUMN update_manifest TEXT;
ALTER TABLE releases ADD COLUMN update_signature TEXT;
ALTER TABLE releases ADD COLUMN installer TEXT;
ALTER TABLE releases ADD COLUMN notes TEXT;
ALTER TABLE releases ADD COLUMN offered INTEGER NOT NULL DEFAULT 0;
