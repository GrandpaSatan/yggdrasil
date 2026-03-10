use thiserror::Error;
use ygg_embed::EmbedError;
use ygg_store::error::StoreError;

/// Unified error type for the Huginn indexer.
#[derive(Debug, Error)]
pub enum HuginnError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("embedding error: {0}")]
    Embed(#[from] EmbedError),

    #[error("config error: {0}")]
    Config(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("watcher error: {0}")]
    Watch(String),

    #[error("internal error: {0}")]
    Internal(String),
}
