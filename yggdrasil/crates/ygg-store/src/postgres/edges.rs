//! CRUD + traversal operations for the `yggdrasil.engram_edges` table.
//!
//! Provides a typed, directed graph over engrams:
//!   - `link` — create an edge between two engrams
//!   - `unlink` — remove an edge
//!   - `neighbors` — get edges from a source (outgoing) or to a target (incoming)
//!   - `traverse` — BFS traversal up to N hops from a starting engram

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::StoreError;

/// An edge row from the database.
#[derive(Debug, Clone)]
pub struct Edge {
    pub id: Uuid,
    pub source_id: Uuid,
    pub target_id: Uuid,
    pub relation: String,
    pub weight: f32,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Create a directed edge between two engrams.
/// Returns the edge UUID. Fails with Duplicate if the edge already exists.
pub async fn link(
    pool: &PgPool,
    source_id: Uuid,
    target_id: Uuid,
    relation: &str,
    weight: f32,
    metadata: Option<serde_json::Value>,
) -> Result<Uuid, StoreError> {
    let meta = metadata.unwrap_or(serde_json::json!({}));

    let id: (Uuid,) = sqlx::query_as(
        "INSERT INTO yggdrasil.engram_edges (source_id, target_id, relation, weight, metadata)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(source_id)
    .bind(target_id)
    .bind(relation)
    .bind(weight)
    .bind(&meta)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("unique_edge") || e.to_string().contains("duplicate") {
            StoreError::Duplicate(format!(
                "edge already exists: {} --[{}]--> {}",
                source_id, relation, target_id
            ))
        } else {
            StoreError::Query(e.to_string())
        }
    })?;

    Ok(id.0)
}

/// Remove an edge by source, target, and relation.
pub async fn unlink(
    pool: &PgPool,
    source_id: Uuid,
    target_id: Uuid,
    relation: &str,
) -> Result<bool, StoreError> {
    let rows = sqlx::query(
        "DELETE FROM yggdrasil.engram_edges
         WHERE source_id = $1 AND target_id = $2 AND relation = $3",
    )
    .bind(source_id)
    .bind(target_id)
    .bind(relation)
    .execute(pool)
    .await
    .map_err(|e| StoreError::Query(e.to_string()))?;

    Ok(rows.rows_affected() > 0)
}

/// Direction for neighbor queries.
pub enum Direction {
    /// Edges going out from the node.
    Outgoing,
    /// Edges coming in to the node.
    Incoming,
    /// Both directions.
    Both,
}

/// Get edges connected to an engram.
pub async fn neighbors(
    pool: &PgPool,
    engram_id: Uuid,
    direction: Direction,
    relation: Option<&str>,
    limit: u32,
) -> Result<Vec<Edge>, StoreError> {
    let (sql, bind_relation) = match direction {
        Direction::Outgoing => {
            if relation.is_some() {
                (
                    "SELECT id, source_id, target_id, relation, weight, metadata, created_at
                     FROM yggdrasil.engram_edges
                     WHERE source_id = $1 AND relation = $2
                     ORDER BY weight DESC, created_at DESC
                     LIMIT $3",
                    true,
                )
            } else {
                (
                    "SELECT id, source_id, target_id, relation, weight, metadata, created_at
                     FROM yggdrasil.engram_edges
                     WHERE source_id = $1
                     ORDER BY weight DESC, created_at DESC
                     LIMIT $2",
                    false,
                )
            }
        }
        Direction::Incoming => {
            if relation.is_some() {
                (
                    "SELECT id, source_id, target_id, relation, weight, metadata, created_at
                     FROM yggdrasil.engram_edges
                     WHERE target_id = $1 AND relation = $2
                     ORDER BY weight DESC, created_at DESC
                     LIMIT $3",
                    true,
                )
            } else {
                (
                    "SELECT id, source_id, target_id, relation, weight, metadata, created_at
                     FROM yggdrasil.engram_edges
                     WHERE target_id = $1
                     ORDER BY weight DESC, created_at DESC
                     LIMIT $2",
                    false,
                )
            }
        }
        Direction::Both => {
            if relation.is_some() {
                (
                    "SELECT id, source_id, target_id, relation, weight, metadata, created_at
                     FROM yggdrasil.engram_edges
                     WHERE (source_id = $1 OR target_id = $1) AND relation = $2
                     ORDER BY weight DESC, created_at DESC
                     LIMIT $3",
                    true,
                )
            } else {
                (
                    "SELECT id, source_id, target_id, relation, weight, metadata, created_at
                     FROM yggdrasil.engram_edges
                     WHERE (source_id = $1 OR target_id = $1)
                     ORDER BY weight DESC, created_at DESC
                     LIMIT $2",
                    false,
                )
            }
        }
    };

    let rows = if bind_relation {
        sqlx::query_as::<_, EdgeRow>(sql)
            .bind(engram_id)
            .bind(relation.unwrap_or(""))
            .bind(limit as i64)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as::<_, EdgeRow>(sql)
            .bind(engram_id)
            .bind(limit as i64)
            .fetch_all(pool)
            .await
    };

    rows.map(|rs| rs.into_iter().map(Into::into).collect())
        .map_err(|e| StoreError::Query(e.to_string()))
}

/// BFS traversal from a starting engram, up to `max_depth` hops.
/// Returns all edges discovered within the traversal.
pub async fn traverse(
    pool: &PgPool,
    start_id: Uuid,
    max_depth: u32,
    relation: Option<&str>,
    limit: u32,
) -> Result<Vec<Edge>, StoreError> {
    // Use a recursive CTE for BFS.
    let sql = if relation.is_some() {
        "WITH RECURSIVE graph AS (
            SELECT id, source_id, target_id, relation, weight, metadata, created_at, 1 AS depth
            FROM yggdrasil.engram_edges
            WHERE source_id = $1 AND relation = $3
          UNION ALL
            SELECT e.id, e.source_id, e.target_id, e.relation, e.weight, e.metadata, e.created_at, g.depth + 1
            FROM yggdrasil.engram_edges e
            JOIN graph g ON e.source_id = g.target_id
            WHERE g.depth < $2 AND e.relation = $3
        )
        SELECT DISTINCT id, source_id, target_id, relation, weight, metadata, created_at
        FROM graph
        ORDER BY created_at DESC
        LIMIT $4"
    } else {
        "WITH RECURSIVE graph AS (
            SELECT id, source_id, target_id, relation, weight, metadata, created_at, 1 AS depth
            FROM yggdrasil.engram_edges
            WHERE source_id = $1
          UNION ALL
            SELECT e.id, e.source_id, e.target_id, e.relation, e.weight, e.metadata, e.created_at, g.depth + 1
            FROM yggdrasil.engram_edges e
            JOIN graph g ON e.source_id = g.target_id
            WHERE g.depth < $2
        )
        SELECT DISTINCT id, source_id, target_id, relation, weight, metadata, created_at
        FROM graph
        ORDER BY created_at DESC
        LIMIT $3"
    };

    let rows = if let Some(rel) = relation {
        sqlx::query_as::<_, EdgeRow>(sql)
            .bind(start_id)
            .bind(max_depth as i32)
            .bind(rel)
            .bind(limit as i64)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query_as::<_, EdgeRow>(sql)
            .bind(start_id)
            .bind(max_depth as i32)
            .bind(limit as i64)
            .fetch_all(pool)
            .await
    };

    rows.map(|rs| rs.into_iter().map(Into::into).collect())
        .map_err(|e| StoreError::Query(e.to_string()))
}

// ---------------------------------------------------------------------------
// Internal row type
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct EdgeRow {
    id: Uuid,
    source_id: Uuid,
    target_id: Uuid,
    relation: String,
    weight: f32,
    metadata: serde_json::Value,
    created_at: DateTime<Utc>,
}

impl From<EdgeRow> for Edge {
    fn from(r: EdgeRow) -> Self {
        Self {
            id: r.id,
            source_id: r.source_id,
            target_id: r.target_id,
            relation: r.relation,
            weight: r.weight,
            metadata: r.metadata,
            created_at: r.created_at,
        }
    }
}
