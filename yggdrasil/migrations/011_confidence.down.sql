-- Revert Sprint 055 confidence scoring.
ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS confidence;
