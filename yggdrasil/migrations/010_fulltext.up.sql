-- Sprint 055: BM25-style full-text search for keyword/exact-match recall.
-- Adds a generated tsvector column and GIN index for fast keyword lookups.
-- This enables System 3 (keyword) alongside System 1 (Hamming) and System 2 (Qdrant vector).

ALTER TABLE yggdrasil.engrams
    ADD COLUMN IF NOT EXISTS tsv tsvector
    GENERATED ALWAYS AS (to_tsvector('english', coalesce(cause, '') || ' ' || coalesce(effect, ''))) STORED;

CREATE INDEX IF NOT EXISTS idx_engrams_fulltext
    ON yggdrasil.engrams USING GIN(tsv);
