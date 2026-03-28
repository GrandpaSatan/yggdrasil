-- Sprint 049: Project isolation for multi-tenant memory
-- Adds project/scope columns to engrams, vault table, and project registry.

-- Add project column to engrams (nullable — NULL means global/unscoped)
ALTER TABLE yggdrasil.engrams ADD COLUMN IF NOT EXISTS project TEXT;

-- Add scope column: 'global', 'project', 'user:<name>' — strict separation from project
ALTER TABLE yggdrasil.engrams ADD COLUMN IF NOT EXISTS scope TEXT NOT NULL DEFAULT 'global';

-- Index for project-scoped queries
CREATE INDEX IF NOT EXISTS idx_engrams_project ON yggdrasil.engrams(project);

-- Composite index for project + tier (most common query pattern)
CREATE INDEX IF NOT EXISTS idx_engrams_project_tier ON yggdrasil.engrams(project, tier);

-- Index for scope-based queries (user memories, global lookups)
CREATE INDEX IF NOT EXISTS idx_engrams_scope ON yggdrasil.engrams(scope);

-- Environment vault: encrypted secrets (AES-256-GCM)
CREATE TABLE IF NOT EXISTS yggdrasil.vault (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    key_name TEXT NOT NULL,
    encrypted_value BYTEA NOT NULL,
    scope TEXT NOT NULL DEFAULT 'global',
    tags TEXT[] DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    CONSTRAINT unique_key_scope UNIQUE (key_name, scope)
);

-- Project registry: tracks known projects and their state
CREATE TABLE IF NOT EXISTS yggdrasil.projects (
    name TEXT PRIMARY KEY,
    display_name TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    last_active TIMESTAMPTZ DEFAULT NOW(),
    engram_count BIGINT DEFAULT 0
);
