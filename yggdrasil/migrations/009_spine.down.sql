-- Revert Sprint 055 spine extensions.
DROP INDEX IF EXISTS yggdrasil.idx_tasks_spine_label;
ALTER TABLE yggdrasil.tasks DROP COLUMN IF EXISTS ttl_secs;
ALTER TABLE yggdrasil.tasks DROP COLUMN IF EXISTS context;
ALTER TABLE yggdrasil.tasks DROP COLUMN IF EXISTS label;
