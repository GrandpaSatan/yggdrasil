CREATE TABLE IF NOT EXISTS yggdrasil.indexed_files (
    file_path TEXT PRIMARY KEY,
    content_hash BYTEA NOT NULL,
    language TEXT NOT NULL,
    chunk_count INTEGER NOT NULL DEFAULT 0,
    indexed_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS yggdrasil.code_chunks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    file_path TEXT NOT NULL REFERENCES yggdrasil.indexed_files(file_path) ON DELETE CASCADE,
    repo_root TEXT NOT NULL,
    language TEXT NOT NULL,
    chunk_type TEXT NOT NULL,
    name TEXT NOT NULL,
    parent_context TEXT DEFAULT '',
    content TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    content_hash BYTEA NOT NULL,
    indexed_at TIMESTAMPTZ DEFAULT NOW(),
    -- tsvector for BM25 full-text search
    search_vec tsvector GENERATED ALWAYS AS (
        to_tsvector('english', name || ' ' || coalesce(parent_context, '') || ' ' || content)
    ) STORED
);

CREATE INDEX IF NOT EXISTS idx_chunks_file
    ON yggdrasil.code_chunks(file_path);

CREATE INDEX IF NOT EXISTS idx_chunks_language
    ON yggdrasil.code_chunks(language);

CREATE INDEX IF NOT EXISTS idx_chunks_type
    ON yggdrasil.code_chunks(chunk_type);

CREATE INDEX IF NOT EXISTS idx_chunks_search_vec
    ON yggdrasil.code_chunks USING gin(search_vec);
