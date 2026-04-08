-- Sprint 055: Spine message queue extensions for model worker routing.
-- Adds label-based task routing, structured context payloads, and TTL.

ALTER TABLE yggdrasil.tasks ADD COLUMN IF NOT EXISTS label TEXT;
ALTER TABLE yggdrasil.tasks ADD COLUMN IF NOT EXISTS context JSONB;
ALTER TABLE yggdrasil.tasks ADD COLUMN IF NOT EXISTS ttl_secs INT;

-- Index for spine pop: pending tasks by label, ordered by priority.
CREATE INDEX IF NOT EXISTS idx_tasks_spine_label
    ON yggdrasil.tasks (label, status, priority DESC, created_at ASC)
    WHERE status = 'pending' AND label IS NOT NULL;
