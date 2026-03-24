/// Mimir error type — re-exported from `ygg_server::error::ServiceError`.
///
/// All HTTP status code mapping, logging, and JSON serialization is handled
/// by the shared `ServiceError` implementation in `ygg-server`.
pub type MimirError = ygg_server::error::ServiceError;
