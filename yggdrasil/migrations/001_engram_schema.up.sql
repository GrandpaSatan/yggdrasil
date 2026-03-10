CREATE SCHEMA IF NOT EXISTS yggdrasil;

CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE IF NOT EXISTS yggdrasil.engrams (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cause TEXT NOT NULL,
    effect TEXT NOT NULL,
    cause_embedding vector(4096),
    content_hash BYTEA NOT NULL UNIQUE,
    tier TEXT NOT NULL DEFAULT 'recall',
    tags TEXT[] DEFAULT '{}',
    access_count BIGINT DEFAULT 0,
    last_accessed TIMESTAMPTZ DEFAULT NOW(),
    created_at TIMESTAMPTZ DEFAULT NOW(),
    archived_by UUID REFERENCES yggdrasil.engrams(id),
    summary_of UUID[] DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_engrams_tier
    ON yggdrasil.engrams(tier);

-- pgvector IVFFlat/HNSW capped at 2000 dims; Qdrant handles vector search.
-- No vector index on cause_embedding — the column is used for fallback queries only.

CREATE INDEX IF NOT EXISTS idx_engrams_created
    ON yggdrasil.engrams(created_at DESC);

CREATE TABLE IF NOT EXISTS yggdrasil.lsh_buckets (
    table_idx SMALLINT NOT NULL,
    bucket_hash BIGINT NOT NULL,
    engram_id UUID NOT NULL REFERENCES yggdrasil.engrams(id) ON DELETE CASCADE,
    PRIMARY KEY (table_idx, bucket_hash, engram_id)
);

CREATE INDEX IF NOT EXISTS idx_lsh_lookup
    ON yggdrasil.lsh_buckets(table_idx, bucket_hash);
