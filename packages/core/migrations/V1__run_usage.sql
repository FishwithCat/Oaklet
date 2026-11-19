-- Provider-reported token usage for one run. Existing rows read as zero; the
-- migration intentionally does not backfill.
ALTER TABLE runs ADD COLUMN prompt_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN completion_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN cache_hit_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN cache_miss_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN model_steps INTEGER NOT NULL DEFAULT 0;
