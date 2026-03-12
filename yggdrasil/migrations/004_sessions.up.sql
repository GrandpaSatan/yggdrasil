-- Migration 004: MCP session persistence
-- Enables session metadata survival across server restarts.
-- Session transport state (SSE channels, event caches) is still in-memory;
-- this table stores semantic context (SDR state, project, last query) that
-- can be carried over to new sessions.

CREATE TABLE IF NOT EXISTS yggdrasil.sessions (
    session_id    UUID PRIMARY KEY,
    client_name   TEXT NOT NULL DEFAULT 'unknown',
    project_id    TEXT,
    state_json    JSONB NOT NULL DEFAULT '{}',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at    TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '24 hours'
);

CREATE INDEX IF NOT EXISTS idx_sessions_expires ON yggdrasil.sessions (expires_at);
CREATE INDEX IF NOT EXISTS idx_sessions_project ON yggdrasil.sessions (project_id);
CREATE INDEX IF NOT EXISTS idx_sessions_last_seen ON yggdrasil.sessions (last_seen DESC);
