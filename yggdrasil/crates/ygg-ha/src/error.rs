/// Home Assistant integration errors.
#[derive(Debug, thiserror::Error)]
pub enum HaError {
    #[error("HA HTTP error: {0}")]
    Http(String),
    #[error("HA API error: {0}")]
    Api(String),
    #[error("HA parse error: {0}")]
    Parse(String),
    #[error("HA not configured")]
    NotConfigured,
    #[error("HA request timed out: {0}")]
    Timeout(String),
    #[error("HA automation generation failed: {0}")]
    Generation(String),
}
