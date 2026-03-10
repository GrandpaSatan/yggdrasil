/// Domain-level errors (no I/O, no network).
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("invalid memory tier: {0}")]
    InvalidMemoryTier(String),

    #[error("invalid language: {0}")]
    InvalidLanguage(String),

    #[error("invalid chunk type: {0}")]
    InvalidChunkType(String),

    #[error("config validation: {0}")]
    ConfigValidation(String),

    #[error("serialization: {0}")]
    Serialization(String),
}
