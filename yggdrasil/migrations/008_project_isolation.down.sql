-- Rollback Sprint 049: Project isolation
DROP TABLE IF EXISTS yggdrasil.projects;
DROP TABLE IF EXISTS yggdrasil.vault;
DROP INDEX IF EXISTS yggdrasil.idx_engrams_scope;
DROP INDEX IF EXISTS yggdrasil.idx_engrams_project_tier;
DROP INDEX IF EXISTS yggdrasil.idx_engrams_project;
ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS scope;
ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS project;
