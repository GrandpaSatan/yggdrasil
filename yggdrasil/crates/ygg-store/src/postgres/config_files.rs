use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::StoreError;

/// A row from the `yggdrasil.config_files` table.
#[derive(Debug, Clone, FromRow)]
pub struct ConfigFileRow {
    pub id: Uuid,
    pub file_type: String,
    pub project_id: Option<String>,
    pub content: String,
    pub content_hash: String,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A row from the `yggdrasil.version_info` table.
#[derive(Debug, Clone, FromRow)]
pub struct VersionRow {
    pub component: String,
    pub version: String,
    pub release_notes: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Upsert a config file. Uses the unique index on (file_type, COALESCE(project_id, '__global__')).
pub async fn upsert_config(
    pool: &PgPool,
    file_type: &str,
    project_id: Option<&str>,
    content: &str,
    content_hash: &str,
    updated_by: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        r#"
        INSERT INTO yggdrasil.config_files (file_type, project_id, content, content_hash, updated_by)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (file_type, COALESCE(project_id, '__global__'))
        DO UPDATE SET
            content = EXCLUDED.content,
            content_hash = EXCLUDED.content_hash,
            updated_by = EXCLUDED.updated_by,
            updated_at = NOW()
        "#,
    )
    .bind(file_type)
    .bind(project_id)
    .bind(content)
    .bind(content_hash)
    .bind(updated_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch a single config file by type and optional project.
pub async fn get_config(
    pool: &PgPool,
    file_type: &str,
    project_id: Option<&str>,
) -> Result<Option<ConfigFileRow>, StoreError> {
    let row = sqlx::query_as::<_, ConfigFileRow>(
        r#"
        SELECT id, file_type, project_id, content, content_hash, updated_by, created_at, updated_at
        FROM yggdrasil.config_files
        WHERE file_type = $1 AND COALESCE(project_id, '__global__') = COALESCE($2, '__global__')
        "#,
    )
    .bind(file_type)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Fetch all config files — global ones plus any matching the given project.
pub async fn get_all_configs(
    pool: &PgPool,
    project_id: Option<&str>,
) -> Result<Vec<ConfigFileRow>, StoreError> {
    let rows = sqlx::query_as::<_, ConfigFileRow>(
        r#"
        SELECT id, file_type, project_id, content, content_hash, updated_by, created_at, updated_at
        FROM yggdrasil.config_files
        WHERE project_id IS NULL OR project_id = $1
        ORDER BY file_type
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Get version info for a component.
pub async fn get_version(
    pool: &PgPool,
    component: &str,
) -> Result<Option<VersionRow>, StoreError> {
    let row = sqlx::query_as::<_, VersionRow>(
        r#"
        SELECT component, version, release_notes, updated_at
        FROM yggdrasil.version_info
        WHERE component = $1
        "#,
    )
    .bind(component)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Set version directly (upsert).
pub async fn set_version(
    pool: &PgPool,
    component: &str,
    version: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        r#"
        INSERT INTO yggdrasil.version_info (component, version)
        VALUES ($1, $2)
        ON CONFLICT (component)
        DO UPDATE SET version = EXCLUDED.version, updated_at = NOW()
        "#,
    )
    .bind(component)
    .bind(version)
    .execute(pool)
    .await?;
    Ok(())
}

/// Bump version by "minor" or "patch". Parses semver manually.
pub async fn bump_version(
    pool: &PgPool,
    component: &str,
    bump_type: &str,
) -> Result<String, StoreError> {
    let current = get_version(pool, component)
        .await?
        .map(|v| v.version)
        .unwrap_or_else(|| "0.0.0".to_string());

    let parts: Vec<&str> = current.split('.').collect();
    if parts.len() != 3 {
        return Err(StoreError::Query(format!(
            "invalid semver for component '{component}': {current}"
        )));
    }

    let major: u32 = parts[0]
        .parse()
        .map_err(|_| StoreError::Query(format!("invalid semver: {current}")))?;
    let minor: u32 = parts[1]
        .parse()
        .map_err(|_| StoreError::Query(format!("invalid semver: {current}")))?;
    let patch: u32 = parts[2]
        .parse()
        .map_err(|_| StoreError::Query(format!("invalid semver: {current}")))?;

    let new_version = match bump_type {
        "patch" => format!("{major}.{minor}.{}", patch + 1),
        "minor" => format!("{major}.{}.0", minor + 1),
        _ => {
            return Err(StoreError::Query(format!(
                "invalid bump_type '{bump_type}': expected 'minor' or 'patch'"
            )));
        }
    };

    set_version(pool, component, &new_version).await?;
    Ok(new_version)
}
