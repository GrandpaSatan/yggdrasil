//! MCP resource implementations for the Yggdrasil system.
//!
//! Provides two resources:
//! - `yggdrasil://models`         — lists available Odin models
//! - `yggdrasil://memory/stats`   — engram tier statistics from Mimir

use reqwest::Client;
use rmcp::model::{ReadResourceResult, ResourceContents};
use std::time::Duration;
use tracing::instrument;
use ygg_domain::config::McpServerConfig;

/// URI constant for the models resource.
pub const RESOURCE_MODELS: &str = "yggdrasil://models";

/// URI constant for the memory statistics resource.
pub const RESOURCE_MEMORY_STATS: &str = "yggdrasil://memory/stats";

// ---------------------------------------------------------------------------
// Internal HTTP response type for Mimir stats
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct StatsResponse {
    total: Option<u64>,
    recall_count: Option<u64>,
    archive_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// Resource: yggdrasil://models
// ---------------------------------------------------------------------------

/// Fetch the model list from Odin and return it as a resource.
#[instrument(skip(client, config))]
pub async fn read_models_resource(
    client: &Client,
    config: &McpServerConfig,
) -> ReadResourceResult {
    // Reuse the formatted table from the tools module.
    let text = crate::tools::models_table(client, config).await;

    ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
        uri: RESOURCE_MODELS.to_string(),
        mime_type: Some("text/plain".to_string()),
        text,
        meta: None,
    }])
}

// ---------------------------------------------------------------------------
// Resource: yggdrasil://memory/stats
// ---------------------------------------------------------------------------

/// Fetch memory tier statistics from Odin (which proxies Mimir).
///
/// If the endpoint does not exist or is unreachable, returns a graceful
/// "not available" message rather than propagating an error.
#[instrument(skip(client, config))]
pub async fn read_memory_stats_resource(
    client: &Client,
    config: &McpServerConfig,
) -> ReadResourceResult {
    let timeout = Duration::from_secs(config.timeout_secs);
    // Mimir's /api/v1/stats is proxied through Odin's /api/v1/stats path.
    // Sprint 004 does not yet define this endpoint; we attempt it optimistically
    // and fall back to the "not available" response per sprint out-of-scope note.
    let url = format!("{}/api/v1/stats", config.odin_url.trim_end_matches('/'));

    let text = match client.get(&url).timeout(timeout).send().await {
        Err(_) => "Memory statistics not available.".to_string(),
        Ok(resp) if !resp.status().is_success() => {
            "Memory statistics not available.".to_string()
        }
        Ok(resp) => match resp.json::<StatsResponse>().await {
            Err(_) => "Memory statistics not available.".to_string(),
            Ok(stats) => {
                let total = stats.total.unwrap_or(0);
                let recall = stats.recall_count.unwrap_or(0);
                let archive = stats.archive_count.unwrap_or(0);
                format!(
                    "## Memory Statistics\n\n\
                     | Tier    | Count |\n\
                     |---------|-------|\n\
                     | Total   | {}    |\n\
                     | Recall  | {}    |\n\
                     | Archive | {}    |",
                    total, recall, archive
                )
            }
        },
    };

    ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
        uri: RESOURCE_MEMORY_STATS.to_string(),
        mime_type: Some("text/plain".to_string()),
        text,
        meta: None,
    }])
}
