-- Revert Sprint 055 full-text search.
DROP INDEX IF EXISTS yggdrasil.idx_engrams_fulltext;
ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS tsv;
