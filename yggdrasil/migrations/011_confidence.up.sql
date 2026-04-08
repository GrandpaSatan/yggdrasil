-- Sprint 055: Confidence scoring for engrams.
-- Auto-ingested: 0.7, manual: 0.9, core: 1.0.
-- Incremented on access (+0.02), halved on contradiction (*0.5).

ALTER TABLE yggdrasil.engrams ADD COLUMN IF NOT EXISTS confidence REAL NOT NULL DEFAULT 0.7;

-- Set existing Core-tier engrams to max confidence.
UPDATE yggdrasil.engrams SET confidence = 1.0 WHERE tier = 'core';
