-- Engram relationship graph: directed, typed edges between engrams.
-- Enables: "related_to", "depends_on", "supersedes", "caused_by", etc.

CREATE TABLE IF NOT EXISTS yggdrasil.engram_edges (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id   UUID NOT NULL REFERENCES yggdrasil.engrams(id) ON DELETE CASCADE,
    target_id   UUID NOT NULL REFERENCES yggdrasil.engrams(id) ON DELETE CASCADE,
    relation    TEXT NOT NULL,   -- edge type: "related_to", "depends_on", "supersedes", "caused_by"
    weight      REAL NOT NULL DEFAULT 1.0,  -- relationship strength (0.0 - 1.0)
    metadata    JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT no_self_loops CHECK (source_id != target_id),
    CONSTRAINT unique_edge UNIQUE (source_id, target_id, relation)
);

-- Index for outgoing edges (given a source, find targets).
CREATE INDEX IF NOT EXISTS idx_edges_source ON yggdrasil.engram_edges (source_id);

-- Index for incoming edges (given a target, find sources).
CREATE INDEX IF NOT EXISTS idx_edges_target ON yggdrasil.engram_edges (target_id);

-- Index for relation-type queries.
CREATE INDEX IF NOT EXISTS idx_edges_relation ON yggdrasil.engram_edges (relation);
