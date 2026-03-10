/// Store-level errors for database operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database connection error: {0}")]
    Connection(String),

    #[error("migration error: {0}")]
    Migration(String),

    #[error("query error: {0}")]
    Query(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("duplicate: {0}")]
    Duplicate(String),

    #[error("qdrant error: {0}")]
    Qdrant(String),
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        Self::Query(e.to_string())
    }
}
