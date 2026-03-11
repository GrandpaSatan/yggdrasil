use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

use crate::error::StoreError;
use ygg_domain::engram::{Engram, MemoryTier, MemoryStats};

/// Retrieve an engram by ID.
pub async fn get_engram(pool: &PgPool, id: Uuid) -> Result<Engram, StoreError> {
    let row = sqlx::query(
        r#"
        SELECT id, cause, effect, tier, tags,
               created_at, access_count, last_accessed
        FROM yggdrasil.engrams
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("engram {id}")))?;

    Ok(Engram {
        id: row.get("id"),
        cause: row.get("cause"),
        effect: row.get("effect"),
        similarity: 0.0,
        tier: parse_tier(row.get::<String, _>("tier").as_str()),
        tags: row.get::<Vec<String>, _>("tags"),
        created_at: row.get::<DateTime<Utc>, _>("created_at"),
        access_count: row.get::<i64, _>("access_count"),
        last_accessed: row.get::<DateTime<Utc>, _>("last_accessed"),
    })
}

/// Delete an engram by ID.
pub async fn delete_engram(pool: &PgPool, id: Uuid) -> Result<(), StoreError> {
    let result = sqlx::query("DELETE FROM yggdrasil.engrams WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("engram {id}")));
    }
    Ok(())
}

/// Fetch full Engram structs for a batch of IDs with pre-computed similarity scores.
///
/// Returns engrams in arbitrary order — the caller is responsible for sorting by
/// similarity.  IDs not found in the database are silently skipped.
pub async fn fetch_engrams_by_ids(
    pool: &PgPool,
    ids: &[Uuid],
    sim_map: &std::collections::HashMap<Uuid, f64>,
) -> Result<Vec<Engram>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query(
        r#"
        SELECT id, cause, effect, tier, tags,
               created_at, access_count, last_accessed
        FROM yggdrasil.engrams
        WHERE id = ANY($1)
        "#,
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let id: Uuid = r.get("id");
            Engram {
                id,
                cause: r.get("cause"),
                effect: r.get("effect"),
                similarity: sim_map.get(&id).copied().unwrap_or(0.0),
                tier: parse_tier(r.get::<String, _>("tier").as_str()),
                tags: r.get::<Vec<String>, _>("tags"),
                created_at: r.get::<DateTime<Utc>, _>("created_at"),
                access_count: r.get::<i64, _>("access_count"),
                last_accessed: r.get::<DateTime<Utc>, _>("last_accessed"),
            }
        })
        .collect())
}

/// Get memory tier counts, oldest Recall timestamp, and Recall capacity placeholder.
///
/// Note: `recall_capacity` is set to 0 here — the caller (handler) injects the
/// configured value from `TierConfig` after fetching stats. This keeps the store
/// layer free of config dependencies.
pub async fn get_stats(pool: &PgPool) -> Result<MemoryStats, StoreError> {
    let row = sqlx::query(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE tier = 'core') AS core_count,
            COUNT(*) FILTER (WHERE tier = 'recall') AS recall_count,
            COUNT(*) FILTER (WHERE tier = 'archival') AS archival_count,
            MIN(created_at) FILTER (WHERE tier = 'recall') AS oldest_recall_at
        FROM yggdrasil.engrams
        "#,
    )
    .fetch_one(pool)
    .await?;

    Ok(MemoryStats {
        core_count: row.get::<i64, _>("core_count"),
        recall_count: row.get::<i64, _>("recall_count"),
        archival_count: row.get::<i64, _>("archival_count"),
        // Placeholder — handlers inject the configured value from TierConfig.
        recall_capacity: 0,
        oldest_recall_at: row.get::<Option<DateTime<Utc>>, _>("oldest_recall_at"),
    })
}

/// Return all Core tier engrams ordered by creation time.
///
/// Sets `similarity = 1.0` on each as a marker indicating "always included".
pub async fn get_core_engrams(pool: &PgPool) -> Result<Vec<ygg_domain::engram::Engram>, StoreError> {
    let rows = sqlx::query(
        r#"
        SELECT id, cause, effect, tier, tags, created_at, access_count, last_accessed
        FROM yggdrasil.engrams
        WHERE tier = 'core'
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ygg_domain::engram::Engram {
            id: r.get("id"),
            cause: r.get("cause"),
            effect: r.get("effect"),
            // Marker value: Core engrams are always included, similarity is not computed.
            similarity: 1.0,
            tier: parse_tier(r.get::<String, _>("tier").as_str()),
            tags: r.get::<Vec<String>, _>("tags"),
            created_at: r.get::<DateTime<Utc>, _>("created_at"),
            access_count: r.get::<i64, _>("access_count"),
            last_accessed: r.get::<DateTime<Utc>, _>("last_accessed"),
        })
        .collect())
}

/// Return the oldest, least-accessed Recall engrams eligible for summarization.
///
/// Eligibility criteria:
/// - `tier = 'recall'`
/// - `archived_by IS NULL` (not already processed)
/// - `created_at < NOW() - min_age_secs` (old enough to be safe to summarize)
///
/// Ordered by `access_count ASC, created_at ASC` to prefer stale low-activity engrams.
pub async fn get_oldest_recall_engrams(
    pool: &PgPool,
    limit: usize,
    min_age_secs: u64,
) -> Result<Vec<ygg_domain::engram::Engram>, StoreError> {
    let rows = sqlx::query(
        r#"
        SELECT id, cause, effect, tier, tags, created_at, access_count, last_accessed
        FROM yggdrasil.engrams
        WHERE tier = 'recall'
          AND archived_by IS NULL
          AND created_at < NOW() - ($2::bigint * INTERVAL '1 second')
        ORDER BY access_count ASC, created_at ASC
        LIMIT $1
        "#,
    )
    .bind(limit as i64)
    .bind(min_age_secs as i64)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ygg_domain::engram::Engram {
            id: r.get("id"),
            cause: r.get("cause"),
            effect: r.get("effect"),
            similarity: 0.0,
            tier: parse_tier(r.get::<String, _>("tier").as_str()),
            tags: r.get::<Vec<String>, _>("tags"),
            created_at: r.get::<DateTime<Utc>, _>("created_at"),
            access_count: r.get::<i64, _>("access_count"),
            last_accessed: r.get::<DateTime<Utc>, _>("last_accessed"),
        })
        .collect())
}

/// Mark a batch of Recall engrams as archived, linking them to the summary engram.
///
/// Only updates engrams that are still in `tier = 'recall'` — guards against
/// double-archival if a concurrent cycle races.
pub async fn archive_engrams(
    pool: &PgPool,
    source_ids: &[Uuid],
    summary_id: Uuid,
) -> Result<(), StoreError> {
    if source_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"
        UPDATE yggdrasil.engrams
        SET tier = 'archival',
            archived_by = $2
        WHERE id = ANY($1)
          AND tier = 'recall'
        "#,
    )
    .bind(source_ids)
    .bind(summary_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Promote an engram to a different tier.
pub async fn set_tier(pool: &PgPool, id: Uuid, tier: MemoryTier) -> Result<(), StoreError> {
    let result = sqlx::query("UPDATE yggdrasil.engrams SET tier = $1 WHERE id = $2")
        .bind(tier.as_str())
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("engram {id}")));
    }
    Ok(())
}

/// Insert a new engram with SDR encoding (Sprint 015).
///
/// Replaces `insert_engram` for SDR-based stores. The `sdr_bits` field is a
/// 32-byte BYTEA column added in migration 003_sdr_events.
/// Returns the generated UUID. Rejects duplicates based on content_hash.
pub async fn insert_engram_sdr(
    pool: &PgPool,
    cause: &str,
    effect: &str,
    sdr_bits: &[u8],
    content_hash: &[u8],
    tier: MemoryTier,
    tags: &[String],
    trigger_type: &str,
    trigger_label: &str,
) -> Result<Uuid, StoreError> {
    let id = Uuid::new_v4();
    let tier_str = tier.as_str();

    sqlx::query(
        r#"
        INSERT INTO yggdrasil.engrams
            (id, cause, effect, sdr_bits, content_hash, tier, tags, trigger_type, trigger_label)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(id)
    .bind(cause)
    .bind(effect)
    .bind(sdr_bits)
    .bind(content_hash)
    .bind(tier_str)
    .bind(tags)
    .bind(trigger_type)
    .bind(trigger_label)
    .execute(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            StoreError::Duplicate("engram with identical content already exists".into())
        } else {
            StoreError::from(e)
        }
    })?;

    Ok(id)
}

/// Fetch event metadata for a batch of engram IDs (no cause/effect text).
///
/// Returns `(id, tier, tags, trigger_type, trigger_label, created_at, access_count)` tuples.
/// Called from the recall handler to build `EngramEvent` responses.
pub async fn fetch_engram_events(
    pool: &PgPool,
    ids: &[Uuid],
) -> Result<Vec<(Uuid, String, Vec<String>, String, String, DateTime<Utc>, i64)>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query(
        r#"
        SELECT id, tier, tags, trigger_type, trigger_label, created_at, access_count
        FROM yggdrasil.engrams
        WHERE id = ANY($1)
        "#,
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<String, _>("tier"),
                r.get::<Vec<String>, _>("tags"),
                r.get::<Option<String>, _>("trigger_type")
                    .unwrap_or_else(|| "pattern".to_string()),
                r.get::<Option<String>, _>("trigger_label")
                    .unwrap_or_default(),
                r.get::<DateTime<Utc>, _>("created_at"),
                r.get::<i64, _>("access_count"),
            )
        })
        .collect())
}

/// Fetch Core tier engrams as events (no cause/effect text).
///
/// Returns `(id, tags, trigger_type, trigger_label, created_at, access_count)` tuples
/// ordered by creation time ascending (oldest first).
pub async fn get_core_engram_events(
    pool: &PgPool,
) -> Result<Vec<(Uuid, Vec<String>, String, String, DateTime<Utc>, i64)>, StoreError> {
    let rows = sqlx::query(
        r#"
        SELECT id, tags, trigger_type, trigger_label, created_at, access_count
        FROM yggdrasil.engrams
        WHERE tier = 'core'
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<Uuid, _>("id"),
                r.get::<Vec<String>, _>("tags"),
                r.get::<Option<String>, _>("trigger_type")
                    .unwrap_or_else(|| "fact".to_string()),
                r.get::<Option<String>, _>("trigger_label")
                    .unwrap_or_default(),
                r.get::<DateTime<Utc>, _>("created_at"),
                r.get::<i64, _>("access_count"),
            )
        })
        .collect())
}

fn parse_tier(s: &str) -> MemoryTier {
    match s {
        "core" => MemoryTier::Core,
        "archival" => MemoryTier::Archival,
        _ => MemoryTier::Recall,
    }
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e {
        db_err.code().map_or(false, |c| c == "23505")
    } else {
        false
    }
}
