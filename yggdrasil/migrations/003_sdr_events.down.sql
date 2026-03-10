-- Rollback Sprint 015 SDR changes
-- NOTE: cause_embedding data is permanently lost. Re-embedding would require Ollama.
ALTER TABLE yggdrasil.engrams ADD COLUMN IF NOT EXISTS cause_embedding vector(4096);

ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS sdr_bits;
ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS trigger_type;
ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS trigger_label;

-- Recreate LSH buckets table
CREATE TABLE IF NOT EXISTS yggdrasil.lsh_buckets (
    table_idx SMALLINT NOT NULL,
    bucket_hash BIGINT NOT NULL,
    engram_id UUID NOT NULL REFERENCES yggdrasil.engrams(id) ON DELETE CASCADE,
    PRIMARY KEY (table_idx, bucket_hash, engram_id)
);

CREATE INDEX IF NOT EXISTS idx_lsh_lookup
    ON yggdrasil.lsh_buckets (table_idx, bucket_hash);
