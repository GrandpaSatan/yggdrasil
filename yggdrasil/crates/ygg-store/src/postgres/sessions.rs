use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::StoreError;

/// A row from the `yggdrasil.sessions` table.
#[derive(Debug, Clone, FromRow)]
pub struct SessionRow {
    pub session_id: Uuid,
    pub client_name: String,
    pub project_id: Option<String>,
    pub state_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Create a new session record.
pub async fn create_session(
    pool: &PgPool,
    session_id: Uuid,
    client_name: &str,
    project_id: Option<&str>,
) -> Result<(), StoreError> {
    sqlx::query(
        r#"
        INSERT INTO yggdrasil.sessions (session_id, client_name, project_id)
        VALUES ($1, $2, $3)
        ON CONFLICT (session_id) DO NOTHING
        "#,
    )
    .bind(session_id)
    .bind(client_name)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch a session by ID, only if not expired.
pub async fn get_session(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionRow>, StoreError> {
    let row: Option<SessionRow> = sqlx::query_as(
        r#"
        SELECT session_id, client_name, project_id,
               state_json, created_at, last_seen, expires_at
        FROM yggdrasil.sessions
        WHERE session_id = $1 AND expires_at > NOW()
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Update last_seen and push expiry forward by 24h.
pub async fn touch_session(pool: &PgPool, session_id: Uuid) -> Result<(), StoreError> {
    sqlx::query(
        r#"
        UPDATE yggdrasil.sessions
        SET last_seen = NOW(), expires_at = NOW() + INTERVAL '24 hours'
        WHERE session_id = $1
        "#,
    )
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update the state_json blob for a session.
pub async fn update_state(
    pool: &PgPool,
    session_id: Uuid,
    state: &serde_json::Value,
) -> Result<(), StoreError> {
    sqlx::query(
        r#"
        UPDATE yggdrasil.sessions
        SET state_json = $2, last_seen = NOW(), expires_at = NOW() + INTERVAL '24 hours'
        WHERE session_id = $1
        "#,
    )
    .bind(session_id)
    .bind(state)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get the most recent session for a project (used to carry over context on reconnect).
pub async fn get_latest_session_for_project(
    pool: &PgPool,
    project_id: &str,
) -> Result<Option<SessionRow>, StoreError> {
    let row: Option<SessionRow> = sqlx::query_as(
        r#"
        SELECT session_id, client_name, project_id,
               state_json, created_at, last_seen, expires_at
        FROM yggdrasil.sessions
        WHERE project_id = $1
        ORDER BY last_seen DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Delete a session.
pub async fn delete_session(pool: &PgPool, session_id: Uuid) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM yggdrasil.sessions WHERE session_id = $1")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete all expired sessions, return count removed.
pub async fn cleanup_expired(pool: &PgPool) -> Result<u64, StoreError> {
    let result = sqlx::query("DELETE FROM yggdrasil.sessions WHERE expires_at < NOW()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
