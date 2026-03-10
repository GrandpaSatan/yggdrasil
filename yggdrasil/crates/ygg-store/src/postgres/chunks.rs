use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::StoreError;
use ygg_domain::chunk::{CodeChunk, IndexedFile, Language};

/// Upsert a tracked file record.
pub async fn upsert_indexed_file(
    pool: &PgPool,
    file_path: &str,
    content_hash: &[u8],
    language: Language,
    chunk_count: i32,
) -> Result<(), StoreError> {
    sqlx::query(
        r#"
        INSERT INTO yggdrasil.indexed_files (file_path, content_hash, language, chunk_count, indexed_at)
        VALUES ($1, $2, $3, $4, NOW())
        ON CONFLICT (file_path)
        DO UPDATE SET content_hash = $2, language = $3, chunk_count = $4, indexed_at = NOW()
        "#,
    )
    .bind(file_path)
    .bind(content_hash)
    .bind(language.as_str())
    .bind(chunk_count)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get a tracked file by path, returns None if not indexed.
pub async fn get_indexed_file(
    pool: &PgPool,
    file_path: &str,
) -> Result<Option<IndexedFile>, StoreError> {
    let row = sqlx::query(
        r#"
        SELECT file_path, content_hash, language, chunk_count, indexed_at
        FROM yggdrasil.indexed_files
        WHERE file_path = $1
        "#,
    )
    .bind(file_path)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| IndexedFile {
        file_path: r.get("file_path"),
        content_hash: r.get("content_hash"),
        language: lang_from_stored(r.get::<String, _>("language").as_str()),
        chunk_count: r.get::<i32, _>("chunk_count"),
        indexed_at: r.get("indexed_at"),
    }))
}

/// Insert a code chunk.
pub async fn insert_chunk(
    pool: &PgPool,
    chunk: &CodeChunk,
) -> Result<(), StoreError> {
    sqlx::query(
        r#"
        INSERT INTO yggdrasil.code_chunks
            (id, file_path, repo_root, language, chunk_type, name,
             parent_context, content, start_line, end_line, content_hash)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        "#,
    )
    .bind(chunk.id)
    .bind(&chunk.file_path)
    .bind(&chunk.repo_root)
    .bind(chunk.language.as_str())
    .bind(chunk.chunk_type.as_str())
    .bind(&chunk.name)
    .bind(&chunk.parent_context)
    .bind(&chunk.content)
    .bind(chunk.start_line as i32)
    .bind(chunk.end_line as i32)
    .bind(&chunk.content_hash)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete all chunks for a file (before re-indexing).
pub async fn delete_chunks_for_file(
    pool: &PgPool,
    file_path: &str,
) -> Result<u64, StoreError> {
    let result = sqlx::query("DELETE FROM yggdrasil.code_chunks WHERE file_path = $1")
        .bind(file_path)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Delete the indexed_files record for a file (call after chunks are cleaned up).
pub async fn delete_indexed_file(pool: &PgPool, file_path: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM yggdrasil.indexed_files WHERE file_path = $1")
        .bind(file_path)
        .execute(pool)
        .await?;
    Ok(())
}

/// Get all chunk IDs for a file (used before Qdrant cleanup during re-index).
pub async fn get_chunk_ids_for_file(
    pool: &PgPool,
    file_path: &str,
) -> Result<Vec<Uuid>, StoreError> {
    let rows = sqlx::query("SELECT id FROM yggdrasil.code_chunks WHERE file_path = $1")
        .bind(file_path)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|r| r.get::<Uuid, _>("id")).collect())
}

/// Full-text BM25 search over code chunks using tsvector.
pub async fn search_bm25(
    pool: &PgPool,
    query: &str,
    limit: usize,
    languages: Option<&[String]>,
) -> Result<Vec<(Uuid, f64)>, StoreError> {
    let rows = if let Some(langs) = languages {
        sqlx::query(
            r#"
            SELECT id, ts_rank(search_vec, websearch_to_tsquery('english', $1)) AS rank
            FROM yggdrasil.code_chunks
            WHERE search_vec @@ websearch_to_tsquery('english', $1)
              AND language = ANY($3)
            ORDER BY rank DESC
            LIMIT $2
            "#,
        )
        .bind(query)
        .bind(limit as i64)
        .bind(langs)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            SELECT id, ts_rank(search_vec, websearch_to_tsquery('english', $1)) AS rank
            FROM yggdrasil.code_chunks
            WHERE search_vec @@ websearch_to_tsquery('english', $1)
            ORDER BY rank DESC
            LIMIT $2
            "#,
        )
        .bind(query)
        .bind(limit as i64)
        .fetch_all(pool)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|r| (r.get::<Uuid, _>("id"), r.get::<f32, _>("rank") as f64))
        .collect())
}

/// Get a chunk by ID.
pub async fn get_chunk(pool: &PgPool, id: Uuid) -> Result<CodeChunk, StoreError> {
    let row = sqlx::query(
        r#"
        SELECT id, file_path, repo_root, language, chunk_type, name,
               parent_context, content, start_line, end_line, content_hash, indexed_at
        FROM yggdrasil.code_chunks
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("chunk {id}")))?;

    Ok(CodeChunk {
        id: row.get("id"),
        file_path: row.get("file_path"),
        repo_root: row.get("repo_root"),
        language: lang_from_stored(row.get::<String, _>("language").as_str()),
        chunk_type: parse_chunk_type(row.get::<String, _>("chunk_type").as_str()),
        name: row.get("name"),
        parent_context: row.get::<Option<String>, _>("parent_context").unwrap_or_default(),
        content: row.get("content"),
        start_line: row.get::<i32, _>("start_line") as usize,
        end_line: row.get::<i32, _>("end_line") as usize,
        content_hash: row.get("content_hash"),
        indexed_at: row.get("indexed_at"),
    })
}

/// Batch fetch chunks by UUID array in a single SQL query.
///
/// Returns chunks in arbitrary order (caller must sort if needed).
/// Missing IDs are silently skipped — no error for IDs not found in PostgreSQL.
pub async fn get_chunks_by_ids(
    pool: &PgPool,
    ids: &[Uuid],
) -> Result<Vec<CodeChunk>, StoreError> {
    if ids.is_empty() {
        return Ok(vec![]);
    }

    let rows = sqlx::query(
        r#"
        SELECT id, file_path, repo_root, language, chunk_type, name,
               parent_context, content, start_line, end_line, content_hash, indexed_at
        FROM yggdrasil.code_chunks
        WHERE id = ANY($1)
        "#,
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| CodeChunk {
            id: row.get("id"),
            file_path: row.get("file_path"),
            repo_root: row.get("repo_root"),
            language: lang_from_stored(row.get::<String, _>("language").as_str()),
            chunk_type: parse_chunk_type(row.get::<String, _>("chunk_type").as_str()),
            name: row.get("name"),
            parent_context: row
                .get::<Option<String>, _>("parent_context")
                .unwrap_or_default(),
            content: row.get("content"),
            start_line: row.get::<i32, _>("start_line") as usize,
            end_line: row.get::<i32, _>("end_line") as usize,
            content_hash: row.get("content_hash"),
            indexed_at: row.get("indexed_at"),
        })
        .collect())
}

/// Count total indexed chunks.
pub async fn count_chunks(pool: &PgPool) -> Result<i64, StoreError> {
    let row = sqlx::query("SELECT COUNT(*) AS cnt FROM yggdrasil.code_chunks")
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("cnt"))
}

/// Count total indexed files.
pub async fn count_indexed_files(pool: &PgPool) -> Result<i64, StoreError> {
    let row = sqlx::query("SELECT COUNT(*) AS cnt FROM yggdrasil.indexed_files")
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("cnt"))
}

/// Get chunk count grouped by language.
///
/// Returns a list of `(language_string, count)` tuples.
pub async fn count_chunks_by_language(pool: &PgPool) -> Result<Vec<(String, i64)>, StoreError> {
    let rows = sqlx::query(
        "SELECT language, COUNT(*) AS cnt FROM yggdrasil.code_chunks GROUP BY language",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("language"), r.get::<i64, _>("cnt")))
        .collect())
}

fn lang_from_stored(s: &str) -> Language {
    match s {
        "rust" => Language::Rust,
        "go" => Language::Go,
        "python" => Language::Python,
        "typescript" => Language::TypeScript,
        "javascript" => Language::JavaScript,
        "markdown" => Language::Markdown,
        "yaml" => Language::Yaml,
        _ => Language::Unknown,
    }
}

fn parse_chunk_type(s: &str) -> ygg_domain::chunk::ChunkType {
    use ygg_domain::chunk::ChunkType;
    match s {
        "function" => ChunkType::Function,
        "struct" => ChunkType::Struct,
        "enum" => ChunkType::Enum,
        "impl" => ChunkType::Impl,
        "trait" => ChunkType::Trait,
        "module" => ChunkType::Module,
        "documentation" => ChunkType::Documentation,
        "config" => ChunkType::Config,
        _ => ChunkType::Function,
    }
}
