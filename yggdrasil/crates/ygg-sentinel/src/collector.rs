use std::time::Duration;

use tracing::debug;

use crate::MonitoredNode;

/// Result of a health check for a single node.
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    pub node: String,
    pub healthy: bool,
    pub error: Option<String>,
    pub response_ms: u64,
}

/// Collects health status from all monitored nodes.
pub struct LogCollector {
    nodes: Vec<MonitoredNode>,
    client: reqwest::Client,
}

impl LogCollector {
    pub fn new(nodes: Vec<MonitoredNode>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");

        Self { nodes, client }
    }

    /// Check health of all monitored nodes concurrently.
    pub async fn check_all(&self) -> Vec<HealthCheckResult> {
        let mut handles = Vec::new();

        for node in &self.nodes {
            let client = self.client.clone();
            let node = node.clone();
            handles.push(tokio::spawn(async move {
                check_node(&client, &node).await
            }));
        }

        let mut results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(result) => results.push(result),
                Err(e) => {
                    results.push(HealthCheckResult {
                        node: "unknown".to_string(),
                        healthy: false,
                        error: Some(format!("task panicked: {e}")),
                        response_ms: 0,
                    });
                }
            }
        }
        results
    }
}

async fn check_node(client: &reqwest::Client, node: &MonitoredNode) -> HealthCheckResult {
    let start = std::time::Instant::now();

    match client.get(&node.health_url).send().await {
        Ok(resp) => {
            let elapsed = start.elapsed().as_millis() as u64;
            let healthy = resp.status().is_success();

            debug!(
                node = %node.name,
                status = resp.status().as_u16(),
                ms = elapsed,
                "health check complete"
            );

            HealthCheckResult {
                node: node.name.clone(),
                healthy,
                error: if healthy {
                    None
                } else {
                    Some(format!("HTTP {}", resp.status()))
                },
                response_ms: elapsed,
            }
        }
        Err(e) => {
            let elapsed = start.elapsed().as_millis() as u64;
            HealthCheckResult {
                node: node.name.clone(),
                healthy: false,
                error: Some(e.to_string()),
                response_ms: elapsed,
            }
        }
    }
}
