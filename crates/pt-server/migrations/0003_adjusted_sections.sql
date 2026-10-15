-- Up to five adjusted-scale sections per manual job (PUBLIC_API §6): an
-- adjusted-scale image says which section (0-4, as submitted) it shows.
ALTER TABLE evidence ADD COLUMN section INTEGER;
