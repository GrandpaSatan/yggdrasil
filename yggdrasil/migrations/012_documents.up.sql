-- Sprint 056: Document ingestion table for research flow hybrid search.
-- Stores chunked documents (PDF text, markdown, plain text, transcripts)
-- alongside a tsvector column for PostgreSQL full-text (BM25-style) search.

CREATE TABLE IF NOT EXISTS yggdrasil.documents (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_uri      TEXT NOT NULL,
    doc_type        TEXT NOT NULL,
    title           TEXT,
    chunk_index     INT NOT NULL DEFAULT 0,
    content         TEXT NOT NULL,
    content_hash    BYTEA NOT NULL,
    metadata        JSONB DEFAULT '{}',
    project         TEXT,
    ingested_at     TIMESTAMPTZ DEFAULT now(),
    search_vec      tsvector GENERATED ALWAYS AS (
        to_tsvector('english', content)
    ) STORED,
    UNIQUE(content_hash)
);

CREATE INDEX IF NOT EXISTS idx_documents_source
    ON yggdrasil.documents (source_uri);

CREATE INDEX IF NOT EXISTS idx_documents_project
    ON yggdrasil.documents (project);

CREATE INDEX IF NOT EXISTS idx_documents_ingested
    ON yggdrasil.documents (ingested_at DESC);

CREATE INDEX IF NOT EXISTS idx_documents_fulltext
    ON yggdrasil.documents USING gin(search_vec);
