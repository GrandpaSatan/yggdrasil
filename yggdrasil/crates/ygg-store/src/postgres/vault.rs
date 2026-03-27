//! CRUD operations for the encrypted vault table (`yggdrasil.vault`).
//!
//! Secrets are stored as AES-256-GCM encrypted BYTEA. Encryption/decryption
//! is handled by the caller (mimir::vault) — this module only does DB I/O.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

use crate::error::StoreError;

/// A vault entry as stored in PostgreSQL.
pub struct VaultEntry {
    pub id: Uuid,
    pub key_name: String,
    pub encrypted_value: Vec<u8>,
    pub scope: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Store or update a secret. Uses upsert on (key_name, scope).
pub async fn set_secret(
    pool: &PgPool,
    key_name: &str,
    encrypted_value: &[u8],
    scope: &str,
    tags: &[String],
) -> Result<Uuid, StoreError> {
    let row = sqlx::query(
        r#"
        INSERT INTO yggdrasil.vault (key_name, encrypted_value, scope, tags)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT ON CONSTRAINT unique_key_scope
        DO UPDATE SET encrypted_value = $2, tags = $4, updated_at = NOW()
        RETURNING id
        "#,
    )
    .bind(key_name)
    .bind(encrypted_value)
    .bind(scope)
    .bind(tags)
    .fetch_one(pool)
    .await?;

    Ok(row.get("id"))
}

/// Retrieve a secret by key name and scope.
pub async fn get_secret(
    pool: &PgPool,
    key_name: &str,
    scope: &str,
) -> Result<Option<VaultEntry>, StoreError> {
    let row = sqlx::query(
        r#"
        SELECT id, key_name, encrypted_value, scope, tags, created_at, updated_at
        FROM yggdrasil.vault
        WHERE key_name = $1 AND scope = $2
        "#,
    )
    .bind(key_name)
    .bind(scope)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| VaultEntry {
        id: r.get("id"),
        key_name: r.get("key_name"),
        encrypted_value: r.get("encrypted_value"),
        scope: r.get("scope"),
        tags: r.get::<Vec<String>, _>("tags"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }))
}

/// List all secrets in a scope (returns metadata only, not decrypted values).
pub async fn list_secrets(
    pool: &PgPool,
    scope: Option<&str>,
) -> Result<Vec<VaultListEntry>, StoreError> {
    let rows = if let Some(s) = scope {
        sqlx::query(
            r#"
            SELECT id, key_name, scope, tags, created_at, updated_at
            FROM yggdrasil.vault
            WHERE scope = $1
            ORDER BY key_name ASC
            "#,
        )
        .bind(s)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            SELECT id, key_name, scope, tags, created_at, updated_at
            FROM yggdrasil.vault
            ORDER BY scope ASC, key_name ASC
            "#,
        )
        .fetch_all(pool)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|r| VaultListEntry {
            id: r.get("id"),
            key_name: r.get("key_name"),
            scope: r.get("scope"),
            tags: r.get::<Vec<String>, _>("tags"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        })
        .collect())
}

/// Delete a secret by key name and scope.
pub async fn delete_secret(
    pool: &PgPool,
    key_name: &str,
    scope: &str,
) -> Result<bool, StoreError> {
    let result = sqlx::query(
        "DELETE FROM yggdrasil.vault WHERE key_name = $1 AND scope = $2",
    )
    .bind(key_name)
    .bind(scope)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Metadata-only entry for list operations (no encrypted_value).
pub struct VaultListEntry {
    pub id: Uuid,
    pub key_name: String,
    pub scope: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
