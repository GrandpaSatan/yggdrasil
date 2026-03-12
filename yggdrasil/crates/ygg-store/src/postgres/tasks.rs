//! CRUD operations for the `yggdrasil.tasks` table.
//!
//! Provides a persistent task queue for agent coordination:
//!   - `push` — create a new pending task
//!   - `pop` — atomically claim the highest-priority pending task
//!   - `complete` — mark a task as completed/failed with a result
//!   - `cancel` — mark a task as cancelled
//!   - `list` — query tasks with filters (status, project, agent)
//!   - `get` — fetch a single task by ID

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::StoreError;

/// A task row from the database.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: i32,
    pub agent: Option<String>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub result: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Push a new task onto the queue. Returns the new task's UUID.
pub async fn push(
    pool: &PgPool,
    title: &str,
    description: &str,
    priority: i32,
    project: Option<&str>,
    tags: &[String],
) -> Result<Uuid, StoreError> {
    let id: (Uuid,) = sqlx::query_as(
        "INSERT INTO yggdrasil.tasks (title, description, priority, project, tags)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(title)
    .bind(description)
    .bind(priority)
    .bind(project)
    .bind(tags)
    .fetch_one(pool)
    .await
    .map_err(|e| StoreError::Query(e.to_string()))?;

    Ok(id.0)
}

/// Atomically claim the highest-priority pending task for the given agent.
/// Returns `None` if no pending tasks are available.
pub async fn pop(
    pool: &PgPool,
    agent: &str,
    project: Option<&str>,
) -> Result<Option<Task>, StoreError> {
    // Use a CTE with FOR UPDATE SKIP LOCKED for safe concurrent access.
    let query = if project.is_some() {
        "WITH next AS (
            SELECT id FROM yggdrasil.tasks
            WHERE status = 'pending' AND project = $2
            ORDER BY priority DESC, created_at ASC
            LIMIT 1
            FOR UPDATE SKIP LOCKED
        )
        UPDATE yggdrasil.tasks t
        SET status = 'in_progress', agent = $1, updated_at = now()
        FROM next
        WHERE t.id = next.id
        RETURNING t.id, t.title, t.description, t.status, t.priority,
                  t.agent, t.project, t.tags, t.result,
                  t.created_at, t.updated_at, t.completed_at"
    } else {
        "WITH next AS (
            SELECT id FROM yggdrasil.tasks
            WHERE status = 'pending'
            ORDER BY priority DESC, created_at ASC
            LIMIT 1
            FOR UPDATE SKIP LOCKED
        )
        UPDATE yggdrasil.tasks t
        SET status = 'in_progress', agent = $1, updated_at = now()
        FROM next
        WHERE t.id = next.id
        RETURNING t.id, t.title, t.description, t.status, t.priority,
                  t.agent, t.project, t.tags, t.result,
                  t.created_at, t.updated_at, t.completed_at"
    };

    let row = if let Some(proj) = project {
        sqlx::query_as::<_, TaskRow>(query)
            .bind(agent)
            .bind(proj)
            .fetch_optional(pool)
            .await
    } else {
        sqlx::query_as::<_, TaskRow>(query)
            .bind(agent)
            .fetch_optional(pool)
            .await
    };

    row.map(|opt| opt.map(Into::into))
        .map_err(|e| StoreError::Query(e.to_string()))
}

/// Mark a task as completed or failed with an optional result message.
pub async fn complete(
    pool: &PgPool,
    task_id: Uuid,
    success: bool,
    result: Option<&str>,
) -> Result<bool, StoreError> {
    let status = if success { "completed" } else { "failed" };
    let rows = sqlx::query(
        "UPDATE yggdrasil.tasks
         SET status = $1, result = $2, updated_at = now(), completed_at = now()
         WHERE id = $3 AND status = 'in_progress'",
    )
    .bind(status)
    .bind(result)
    .bind(task_id)
    .execute(pool)
    .await
    .map_err(|e| StoreError::Query(e.to_string()))?;

    Ok(rows.rows_affected() > 0)
}

/// Cancel a task (only if pending or in_progress).
pub async fn cancel(pool: &PgPool, task_id: Uuid) -> Result<bool, StoreError> {
    let rows = sqlx::query(
        "UPDATE yggdrasil.tasks
         SET status = 'cancelled', updated_at = now(), completed_at = now()
         WHERE id = $1 AND status IN ('pending', 'in_progress')",
    )
    .bind(task_id)
    .execute(pool)
    .await
    .map_err(|e| StoreError::Query(e.to_string()))?;

    Ok(rows.rows_affected() > 0)
}

/// Get a single task by ID.
pub async fn get(pool: &PgPool, task_id: Uuid) -> Result<Option<Task>, StoreError> {
    sqlx::query_as::<_, TaskRow>(
        "SELECT id, title, description, status, priority, agent, project,
                tags, result, created_at, updated_at, completed_at
         FROM yggdrasil.tasks WHERE id = $1",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await
    .map(|opt| opt.map(Into::into))
    .map_err(|e| StoreError::Query(e.to_string()))
}

/// List tasks with optional filters.
pub async fn list(
    pool: &PgPool,
    status: Option<&str>,
    project: Option<&str>,
    agent: Option<&str>,
    limit: u32,
) -> Result<Vec<Task>, StoreError> {
    let mut sql = String::from(
        "SELECT id, title, description, status, priority, agent, project,
                tags, result, created_at, updated_at, completed_at
         FROM yggdrasil.tasks WHERE 1=1",
    );
    let mut binds: Vec<String> = Vec::new();
    let mut idx = 1u32;

    if let Some(s) = status {
        sql.push_str(&format!(" AND status = ${idx}"));
        binds.push(s.to_string());
        idx += 1;
    }
    if let Some(p) = project {
        sql.push_str(&format!(" AND project = ${idx}"));
        binds.push(p.to_string());
        idx += 1;
    }
    if let Some(a) = agent {
        sql.push_str(&format!(" AND agent = ${idx}"));
        binds.push(a.to_string());
        idx += 1;
    }

    sql.push_str(&format!(" ORDER BY priority DESC, created_at DESC LIMIT ${idx}"));
    binds.push(limit.to_string());

    // Build the query dynamically.
    let mut q = sqlx::query_as::<_, TaskRow>(&sql);
    for (i, val) in binds.iter().enumerate() {
        // Last bind is the limit (integer), others are text.
        if i == binds.len() - 1 {
            let lim: i64 = val.parse().unwrap_or(20);
            q = q.bind(lim);
        } else {
            q = q.bind(val.as_str());
        }
    }

    q.fetch_all(pool)
        .await
        .map(|rows| rows.into_iter().map(Into::into).collect())
        .map_err(|e| StoreError::Query(e.to_string()))
}

// ---------------------------------------------------------------------------
// Internal row type for sqlx::FromRow
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct TaskRow {
    id: Uuid,
    title: String,
    description: String,
    status: String,
    priority: i32,
    agent: Option<String>,
    project: Option<String>,
    tags: Vec<String>,
    result: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl From<TaskRow> for Task {
    fn from(r: TaskRow) -> Self {
        Self {
            id: r.id,
            title: r.title,
            description: r.description,
            status: r.status,
            priority: r.priority,
            agent: r.agent,
            project: r.project,
            tags: r.tags,
            result: r.result,
            created_at: r.created_at,
            updated_at: r.updated_at,
            completed_at: r.completed_at,
        }
    }
}
