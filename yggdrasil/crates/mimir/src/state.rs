use std::path::Path;
use std::sync::Arc;

use dashmap::DashMap;
use ygg_domain::config::MimirConfig;
use ygg_store::{Store, qdrant::VectorStore};
use ygg_store::qdrant::Distance;

use ygg_embed::OnnxEmbedder;

use crate::{error::MimirError, sdr::Sdr, sdr_index::SdrIndex};

/// SDR collection dimension: 256 bits stored as 256 bipolar floats ({-1.0, 1.0}).
const SDR_DIM: u64 = 256;

/// Name of the v2 SDR collection with payload-based project isolation.
pub const V2_SDR_COLLECTION: &str = "yggdrasil_v2_sdr";

/// Shared application state injected into every Axum handler via `State<Arc<AppState>>`.
///
/// Construction is async — it connects to all external services, loads the ONNX
/// embedder, and creates an empty SDR index. The SDR backfill is deferred to a
/// background task spawned in `main.rs` so the HTTP server can start accepting
/// traffic immediately.
pub struct AppState {
    /// PostgreSQL connection pool (ygg-store wrapper).
    pub store: Store,
    /// Qdrant client for vector operations (System 2 / Archival tier).
    pub vectors: VectorStore,
    /// In-memory SDR index (loaded from sdr_bits on startup, updated on every store).
    pub sdr_index: SdrIndex,
    /// ONNX in-process embedder for SDR encoding.
    pub embedder: OnnxEmbedder,
    /// Loaded YAML configuration.
    pub config: MimirConfig,
    /// Sender to signal background tasks (e.g. summarization) to shut down.
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Per-workstation cooldown map for auto-ingest rate limiting.
    ///
    /// IMPORTANT: Must be Arc<DashMap> — DashMap::clone() creates an independent
    /// deep copy, not a shared reference. Arc ensures all Axum handler clones share
    /// the same underlying map. (Sprint 018 gotcha)
    pub cooldown_map: Arc<DashMap<String, std::time::Instant>>,
    /// Content-hash dedup map: SHA-256 hex → last seen instant.
    /// Same Arc pattern required for shared Axum state.
    pub content_hashes: Arc<DashMap<String, std::time::Instant>>,
    /// Insight template SDRs loaded at startup: (category_name, sdr).
    /// Populated by main.rs after state construction via a PG query for tag "insight_template".
    /// RwLock allows a single background writer at startup and concurrent readers thereafter.
    pub template_sdrs: Arc<std::sync::RwLock<Vec<(String, Sdr)>>>,
    /// Dense 384-dim template embeddings for cosine matching.
    /// Loaded at startup alongside SDR templates. Preserves full magnitude information
    /// that SDR binarization discards, yielding much better classification accuracy.
    pub template_embeddings: Arc<std::sync::RwLock<Vec<(String, Vec<f32>)>>>,
    /// Shared HTTP client for outbound requests (Saga enrichment, etc.).
    /// Reuses connection pool across all fire-and-forget tasks.
    pub http_client: reqwest::Client,
}

impl AppState {
    /// Connect to all external services, load the ONNX embedder, run migrations,
    /// and ensure Qdrant collections. The SDR index is created empty; populate it
    /// from persisted BYTEA rows in a background task after the HTTP server binds.
    pub async fn new(config: MimirConfig) -> Result<Self, MimirError> {
        // --- PostgreSQL ---
        tracing::info!(url = %config.database_url, "connecting to postgresql");
        let store = Store::connect(&config.database_url)
            .await
            .map_err(MimirError::Store)?;

        // Run pending migrations.
        tracing::info!("running migrations from ./migrations");
        store
            .migrate("./migrations")
            .await
            .map_err(MimirError::Store)?;
        tracing::info!("migrations applied");

        // --- Qdrant (System 2: Archival / dense embedding search) ---
        tracing::info!(url = %config.qdrant_url, "connecting to qdrant");
        let vectors = VectorStore::connect(&config.qdrant_url)
            .await
            .map_err(MimirError::Store)?;

        vectors
            .ensure_collection("engrams")
            .await
            .map_err(MimirError::Store)?;
        tracing::info!("qdrant collection 'engrams' ready");

        // Legacy SDR collection (kept for backward compat during migration).
        vectors
            .ensure_collection_dim("engrams_sdr", SDR_DIM, Distance::Dot)
            .await
            .map_err(MimirError::Store)?;
        tracing::info!("qdrant collection 'engrams_sdr' ready (legacy, 256-dim, Dot)");

        // Legacy category collections (kept for backward compat during migration).
        for name in ["sprints", "topology", "projects"] {
            vectors
                .ensure_collection(name)
                .await
                .map_err(MimirError::Store)?;
            tracing::info!("qdrant collection '{name}' ready (legacy, 384-dim, Cosine)");
        }

        // --- V2 SDR collection: single collection with payload-based project isolation ---
        // Uses bipolar {-1.0, 1.0} encoding so Dot product is rank-equivalent to
        // Hamming distance: A'·B' = 256 - 2·H(A,B).
        // Payload fields "project" and "scope" enable O(1) pre-filtering before
        // HNSW traversal — no per-project collection overhead on Hades' N150.
        vectors
            .ensure_collection_dim(V2_SDR_COLLECTION, SDR_DIM, Distance::Dot)
            .await
            .map_err(MimirError::Store)?;
        tracing::info!("qdrant collection '{}' ready (256-dim, Dot, bipolar)", V2_SDR_COLLECTION);

        // Create keyword payload indexes for pre-filtered search.
        vectors
            .create_payload_index(V2_SDR_COLLECTION, "project")
            .await
            .map_err(MimirError::Store)?;
        vectors
            .create_payload_index(V2_SDR_COLLECTION, "scope")
            .await
            .map_err(MimirError::Store)?;
        tracing::info!("payload indexes on 'project' and 'scope' ready for {}", V2_SDR_COLLECTION);

        // --- ONNX Embedder ---
        // Load synchronously at startup. The ONNX session builder is blocking I/O
        // (file read + model parse). We accept this cost at startup to keep the
        // hot path (per-request embed) running on the blocking thread pool.
        tracing::info!(model_dir = %config.sdr.model_dir, "loading ONNX embedder");
        let embedder = OnnxEmbedder::load(Path::new(&config.sdr.model_dir))?;
        tracing::info!("ONNX embedder ready");

        // --- SDR index (System 1: Recall tier fast recall) ---
        let sdr_index = SdrIndex::new();
        tracing::info!(
            dim_bits = config.sdr.dim_bits,
            "sdr index created"
        );

        // --- Shutdown channel ---
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);

        // --- Auto-ingest state (Sprint 044) ---
        // DashMap fields must be Arc-wrapped for Axum shared state — clone() creates
        // an independent copy, not a shared reference. See Sprint 018 gotcha.
        let cooldown_map = Arc::new(DashMap::new());
        let content_hashes = Arc::new(DashMap::new());
        // template_sdrs + template_embeddings populated in main.rs after startup
        // via PG query for "insight_template" tag.
        let template_sdrs = Arc::new(std::sync::RwLock::new(Vec::<(String, Sdr)>::new()));
        let template_embeddings = Arc::new(std::sync::RwLock::new(Vec::<(String, Vec<f32>)>::new()));

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| MimirError::Internal(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            store,
            vectors,
            sdr_index,
            embedder,
            config,
            shutdown_tx,
            cooldown_map,
            content_hashes,
            template_sdrs,
            template_embeddings,
            http_client,
        })
    }
}

/// Load all SDR rows from `yggdrasil.engrams` for Recall-tier engrams.
///
/// Returns `(id, sdr_bits)` pairs. Called once at startup to backfill the in-memory
/// SDR index. Public so that `main.rs` can spawn the backfill as a background task.
pub async fn load_sdr_rows(
    pool: &sqlx::PgPool,
) -> Result<Vec<(uuid::Uuid, Vec<u8>)>, MimirError> {
    use sqlx::Row as _;

    let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
        "SELECT id, sdr_bits FROM yggdrasil.engrams WHERE tier = 'recall' AND sdr_bits IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e: sqlx::Error| MimirError::Store(ygg_store::error::StoreError::Query(e.to_string())))?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let id: uuid::Uuid = r.get("id");
            let sdr_bits: Vec<u8> = r.get("sdr_bits");
            (id, sdr_bits)
        })
        .collect())
}

/// Load all SDR rows with project affiliation for scoped backfill.
///
/// Returns `(id, sdr_bits, project)` triples. The `project` column may be NULL
/// for engrams that predate the project isolation migration (008).
pub async fn load_sdr_rows_scoped(
    pool: &sqlx::PgPool,
) -> Result<Vec<(uuid::Uuid, Vec<u8>, Option<String>)>, MimirError> {
    use sqlx::Row as _;

    let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
        "SELECT id, sdr_bits, project FROM yggdrasil.engrams WHERE tier = 'recall' AND sdr_bits IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e: sqlx::Error| MimirError::Store(ygg_store::error::StoreError::Query(e.to_string())))?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let id: uuid::Uuid = r.get("id");
            let sdr_bits: Vec<u8> = r.get("sdr_bits");
            let project: Option<String> = r.get("project");
            (id, sdr_bits, project)
        })
        .collect())
}

/// Sprint 065 A·P1: load all SDR rows with project + tags for scoped+tagged backfill.
///
/// Returns `(id, sdr_bits, project, tags)` quads. Tags hydrate the parallel
/// tag_index used by `query_scoped_with_tags` to prevent cross-sprint SDR
/// collisions. Used by `main.rs` at startup to populate the in-memory index
/// with full partition-prefix tag metadata.
pub async fn load_sdr_rows_scoped_with_tags(
    pool: &sqlx::PgPool,
) -> Result<Vec<(uuid::Uuid, Vec<u8>, Option<String>, Vec<String>)>, MimirError> {
    use sqlx::Row as _;

    let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
        "SELECT id, sdr_bits, project, tags FROM yggdrasil.engrams WHERE tier = 'recall' AND sdr_bits IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e: sqlx::Error| MimirError::Store(ygg_store::error::StoreError::Query(e.to_string())))?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let id: uuid::Uuid = r.get("id");
            let sdr_bits: Vec<u8> = r.get("sdr_bits");
            let project: Option<String> = r.get("project");
            let tags: Vec<String> = r.try_get("tags").unwrap_or_default();
            (id, sdr_bits, project, tags)
        })
        .collect())
}
