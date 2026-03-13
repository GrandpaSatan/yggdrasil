use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};
use ygg_domain::chunk::Language;
use ygg_domain::config::HuginnConfig;
use ygg_embed::OnnxEmbedder;
use ygg_store::postgres::chunks::{
    delete_chunks_for_file, delete_indexed_file, get_chunk_ids_for_file, get_indexed_file,
    insert_chunk, upsert_indexed_file,
};
use ygg_store::qdrant::VectorStore;
use ygg_store::Store;

use crate::chunker::{build_embed_text, Chunker, MAX_EMBED_CHARS};
use crate::error::HuginnError;

/// Semaphore permit count for concurrent file parsing.
/// Matches half the Ryzen 7 255's 16 hardware threads, leaving headroom for
/// I/O-bound embedding pipeline and OS tasks.
const PARSE_CONCURRENCY: usize = 8;

/// Semaphore permit count for concurrent database writes.
/// Capped to avoid overwhelming the Hades N150 connection pool.
const DB_WRITE_CONCURRENCY: usize = 16;

/// Batch size for embedding requests.
const EMBED_BATCH_SIZE: usize = 10;

/// Directories to skip during file walking. Matched as path component substrings.
pub const IGNORE_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    "vendor",
    ".venv",
];

/// Summary statistics returned after a bulk indexing run.
pub struct IndexStats {
    pub files_scanned: usize,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub chunks_created: usize,
    pub duration: Duration,
}

/// Orchestrates file walking, hash-based change detection, tree-sitter chunking,
/// embedding, and dual-write to PostgreSQL + Qdrant.
pub struct Indexer {
    pub store: Store,
    pub vectors: VectorStore,
    pub embedder: OnnxEmbedder,
    pub config: HuginnConfig,
}

impl Indexer {
    /// Connect to all backend services and ensure the Qdrant collection exists.
    pub async fn new(config: HuginnConfig) -> Result<Self, HuginnError> {
        info!(database_url = %config.database_url, "connecting to PostgreSQL");
        let store = Store::connect(&config.database_url).await?;

        info!("running database migrations");
        store.migrate("./migrations").await?;

        info!(qdrant_url = %config.qdrant_url, "connecting to Qdrant");
        let vectors = VectorStore::connect(&config.qdrant_url).await?;
        vectors.ensure_collection("code_chunks").await?;

        info!(model_dir = %config.embed.model_dir, "loading ONNX embedder");
        let embedder = OnnxEmbedder::load(std::path::Path::new(&config.embed.model_dir))?;

        Ok(Self {
            store,
            vectors,
            embedder,
            config,
        })
    }

    /// Index all files in all configured watch_paths.
    ///
    /// Uses a `Semaphore(PARSE_CONCURRENCY)` to throttle concurrent parse tasks.
    /// Returns aggregate `IndexStats`.
    pub async fn index_all(&self, force: bool) -> Result<IndexStats, HuginnError> {
        let started = Instant::now();
        let parse_sem = Arc::new(Semaphore::new(PARSE_CONCURRENCY));

        let mut files_scanned = 0usize;
        let mut files_indexed = 0usize;
        let mut files_skipped = 0usize;
        let mut chunks_created = 0usize;

        let watch_paths = self.config.watch_paths.clone();

        for watch_path in &watch_paths {
            let repo_root = watch_path.clone();
            info!(path = %repo_root, "walking directory tree");

            // Collect file list first so we can own it across tasks.
            let file_list: Vec<std::path::PathBuf> = collect_files(watch_path);
            files_scanned += file_list.len();

            // Spawn concurrent index tasks bounded by PARSE_CONCURRENCY.
            let mut task_handles = Vec::new();

            for file_path in file_list {
                let sem = Arc::clone(&parse_sem);
                let indexer_store = self.store.clone();
                let indexer_vectors = self.vectors.clone();
                let indexer_embedder = self.embedder.clone();
                let indexer_config = self.config.clone();
                let rr = repo_root.clone();

                let handle = tokio::spawn(async move {
                    let _permit = sem.acquire().await.map_err(|_| {
                        HuginnError::Internal("parse semaphore closed".to_string())
                    })?;
                    let indexer = Indexer {
                        store: indexer_store,
                        vectors: indexer_vectors,
                        embedder: indexer_embedder,
                        config: indexer_config,
                    };
                    indexer.index_file(&file_path, &rr, force).await
                });
                task_handles.push(handle);
            }

            for handle in task_handles {
                match handle.await {
                    Ok(Ok(Some(count))) => {
                        files_indexed += 1;
                        chunks_created += count;
                    }
                    Ok(Ok(None)) => {
                        files_skipped += 1;
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, "file indexing failed — skipping");
                    }
                    Err(e) => {
                        warn!(error = %e, "task panicked during indexing");
                    }
                }
            }
        }

        let duration = started.elapsed();
        let throughput = if duration.as_secs_f64() > 0.0 {
            files_indexed as f64 / (duration.as_secs_f64() / 60.0)
        } else {
            0.0
        };

        info!(
            files_scanned,
            files_indexed,
            files_skipped,
            chunks_created,
            elapsed_s = duration.as_secs_f64(),
            files_per_minute = throughput,
            "indexing complete"
        );

        Ok(IndexStats {
            files_scanned,
            files_indexed,
            files_skipped,
            chunks_created,
            duration,
        })
    }

    /// Index a single file.
    ///
    /// Returns `None` if the file was skipped (hash matches and `!force`).
    /// Returns `Some(chunk_count)` if the file was indexed.
    pub async fn index_file(
        &self,
        path: &Path,
        repo_root: &str,
        force: bool,
    ) -> Result<Option<usize>, HuginnError> {
        let file_path_str = path.to_string_lossy().to_string();

        // Determine language before reading file — skip unknown early.
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = Language::from_extension(extension);
        if matches!(language, Language::Unknown) {
            return Ok(None);
        }

        // Read file content; skip binary files gracefully.
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                debug!(path = %file_path_str, "skipping binary file");
                return Ok(None);
            }
            Err(e) => return Err(HuginnError::Io(e)),
        };

        // Compute SHA-256 of file content.
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let file_hash: Vec<u8> = hasher.finalize().to_vec();

        // Check stored hash — skip if unchanged.
        let existing = get_indexed_file(self.store.pool(), &file_path_str).await?;
        if let Some(ref record) = existing
            && !force && record.content_hash == file_hash {
                debug!(path = %file_path_str, "skipping — hash unchanged");
                return Ok(None);
            }

        info!(path = %file_path_str, language = %language, "indexing file");

        // Delete old chunks for this file (PostgreSQL + Qdrant) before re-indexing.
        if existing.is_some() {
            self.remove_file(&file_path_str).await?;
        }

        // Offload tree-sitter parsing to a blocking thread.
        // tree_sitter::Parser is !Send+!Sync — we construct a new Chunker per task.
        let content_clone = content.clone();
        let file_path_clone = file_path_str.clone();
        let repo_root_clone = repo_root.to_string();

        let chunks = tokio::task::spawn_blocking(move || {
            let mut chunker = Chunker::new()?;
            chunker.chunk_file(&content_clone, language, &file_path_clone, &repo_root_clone)
        })
        .await
        .map_err(|e| HuginnError::Parse(format!("spawn_blocking join error: {e}")))?
        .map_err(|e| HuginnError::Parse(format!("chunking error: {e}")))?;

        let chunk_count = chunks.len();
        debug!(path = %file_path_str, chunks = chunk_count, "parsed chunks");

        // Upsert the indexed_files record BEFORE writing chunks, because
        // code_chunks.file_path has a FK to indexed_files.file_path.
        upsert_indexed_file(
            self.store.pool(),
            &file_path_str,
            &file_hash,
            language,
            chunk_count as i32,
        )
        .await?;

        if chunk_count == 0 {
            return Ok(Some(0));
        }

        // Embed chunks in batches of EMBED_BATCH_SIZE.
        let db_sem = Arc::new(Semaphore::new(DB_WRITE_CONCURRENCY));
        let mut write_handles = Vec::new();

        for batch in chunks.chunks(EMBED_BATCH_SIZE) {
            // Build embedding texts for this batch.
            let embed_texts: Vec<String> = batch
                .iter()
                .map(|c| {
                    let (text, truncated) = build_embed_text(c);
                    if truncated {
                        warn!(
                            chunk_name = %c.name,
                            file = %c.file_path,
                            "chunk content truncated to {MAX_EMBED_CHARS} chars for embedding"
                        );
                    }
                    text
                })
                .collect();

            // Embed with one retry on failure.
            let embeddings = match self.embedder.embed_batch(&embed_texts).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "embedding batch failed, retrying in 1s");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    match self.embedder.embed_batch(&embed_texts).await {
                        Ok(v) => v,
                        Err(e2) => {
                            warn!(error = %e2, "embedding retry failed — skipping batch");
                            continue;
                        }
                    }
                }
            };

            // Persist each chunk + embedding concurrently, throttled by DB_WRITE_CONCURRENCY.
            for (chunk, embedding) in batch.iter().zip(embeddings.into_iter()) {
                let chunk = chunk.clone();
                let pool = self.store.pool().clone();
                let vectors = self.vectors.clone();
                let sem = Arc::clone(&db_sem);

                let handle = tokio::spawn(async move {
                    let _permit = sem.acquire().await.map_err(|_| {
                        HuginnError::Internal("db semaphore closed".to_string())
                    })?;
                    insert_chunk(&pool, &chunk).await?;
                    vectors
                        .upsert(
                            "code_chunks",
                            chunk.id,
                            embedding,
                            std::collections::HashMap::new(),
                        )
                        .await?;
                    Ok::<(), HuginnError>(())
                });
                write_handles.push(handle);
            }
        }

        // Await all write tasks.
        for handle in write_handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!(error = %e, "chunk write failed"),
                Err(e) => warn!(error = %e, "chunk write task panicked"),
            }
        }

        info!(path = %file_path_str, chunks = chunk_count, "indexed");
        Ok(Some(chunk_count))
    }

    /// Delete all PostgreSQL chunks and Qdrant points for a file.
    ///
    /// Called before re-indexing a changed file, and on file-delete events in watch mode.
    pub async fn remove_file(&self, file_path: &str) -> Result<(), HuginnError> {
        let ids = get_chunk_ids_for_file(self.store.pool(), file_path).await?;

        if !ids.is_empty() {
            // Batch-delete from Qdrant first (PostgreSQL CASCADE handles DB cleanup).
            self.vectors.delete_many("code_chunks", &ids).await?;
            debug!(file = %file_path, count = ids.len(), "deleted qdrant points");
        }

        let deleted = delete_chunks_for_file(self.store.pool(), file_path).await?;
        debug!(file = %file_path, rows = deleted, "deleted pg chunks");

        // Also delete the indexed_files record so the file is treated as new next time.
        delete_indexed_file(self.store.pool(), file_path).await?;

        Ok(())
    }
}

// --- File walking -----------------------------------------------------------

/// Walk a directory tree and collect all source files that should be indexed.
/// Applies ignore rules for `.git`, `target`, `node_modules`, etc.
pub fn collect_files(root: &str) -> Vec<std::path::PathBuf> {
    use walkdir::WalkDir;

    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip ignored directories.
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                return !IGNORE_DIRS.iter().any(|d| name == *d);
            }
            true
        })
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().is_file() {
                return None;
            }
            let ext = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let lang = Language::from_extension(ext);
            if matches!(lang, Language::Unknown) {
                return None;
            }
            Some(entry.into_path())
        })
        .collect()
}
