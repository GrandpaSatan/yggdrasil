use sqlx::PgPool;
use ygg_domain::config::SearchConfig;
use ygg_embed::OnnxEmbedder;
use ygg_store::qdrant::VectorStore;

/// Shared application state injected into every Axum handler via `State<AppState>`.
///
/// `AppState` is `Clone` because `PgPool`, `VectorStore`, `OnnxEmbedder`, and `SearchConfig`
/// are all `Clone`. Axum extracts it via `State<AppState>` (no `Arc` wrapping needed —
/// the state is cloned once per handler call, and all inner types are cheaply cloneable
/// reference-counted handles).
#[derive(Clone)]
pub struct AppState {
    /// PostgreSQL connection pool (shared across all handlers).
    pub pool: PgPool,
    /// Qdrant client for vector search.
    pub vectors: VectorStore,
    /// ONNX in-process embedder for query embedding.
    pub embedder: OnnxEmbedder,
    /// Search tuning parameters loaded from YAML config.
    pub search_config: SearchConfig,
}
