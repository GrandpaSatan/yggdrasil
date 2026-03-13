-- Sprint 039.1: Config sync + version tracking tables

CREATE TABLE IF NOT EXISTS yggdrasil.config_files (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    file_type     TEXT NOT NULL,       -- 'global_settings', 'global_claude_md', 'project_settings', 'project_claude_md'
    project_id    TEXT,                -- NULL for global, project name for project-scoped
    content       TEXT NOT NULL,
    content_hash  TEXT NOT NULL,       -- SHA-256 for fast diff
    updated_by    TEXT NOT NULL,       -- workstation hostname
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_config_files_type_project
    ON yggdrasil.config_files (file_type, COALESCE(project_id, '__global__'));

CREATE TABLE IF NOT EXISTS yggdrasil.version_info (
    component     TEXT PRIMARY KEY,    -- 'server', 'client', 'config'
    version       TEXT NOT NULL DEFAULT '1.0.0',
    release_notes TEXT,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO yggdrasil.version_info (component, version) VALUES
    ('server', '1.0.0'), ('client', '1.0.0'), ('config', '1.0.0')
ON CONFLICT DO NOTHING;
