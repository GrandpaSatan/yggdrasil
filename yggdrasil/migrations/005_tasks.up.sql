-- Task queue for persistent agent coordination.
-- Tasks have a lifecycle: pending → in_progress → completed | failed | cancelled.

CREATE TABLE IF NOT EXISTS yggdrasil.tasks (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'in_progress', 'completed', 'failed', 'cancelled')),
    priority    INT  NOT NULL DEFAULT 0,  -- higher = more urgent
    agent       TEXT,                     -- which agent claimed this task
    project     TEXT,                     -- project scope (e.g. 'yggdrasil')
    tags        TEXT[] NOT NULL DEFAULT '{}',
    result      TEXT,                     -- outcome or error message
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

-- Index for queue pop: pending tasks ordered by priority desc, created_at asc.
CREATE INDEX IF NOT EXISTS idx_tasks_queue
    ON yggdrasil.tasks (status, priority DESC, created_at ASC)
    WHERE status = 'pending';

-- Index for project-scoped queries.
CREATE INDEX IF NOT EXISTS idx_tasks_project
    ON yggdrasil.tasks (project, status);
