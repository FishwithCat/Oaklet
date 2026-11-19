-- Usage of the newest model request in a run, so the composer can still show
-- the cache share after a session is reopened. Existing rows read as zero.
ALTER TABLE runs ADD COLUMN last_prompt_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN last_completion_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN last_cache_hit_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN last_cache_miss_tokens INTEGER NOT NULL DEFAULT 0;
