//! DreamerConfig — persisted at `/opt/yggdrasil/config/dreamer.config.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamerConfig {
    /// Upstream Odin URL for /internal/activity polling + chat completions.
    pub odin_url: String,

    /// Upstream Mimir URL for dream-engram persistence.
    pub mimir_url: String,

    /// Idle window (seconds) before warmup / dream flows kick in.
    #[serde(default = "default_min_idle_secs")]
    pub min_idle_secs: u64,

    /// Poll interval against `/internal/activity` (seconds).
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,

    /// Bind address for the dreamer's own HTTP server (health + metrics).
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,

    /// Warmup prefixes — fired every `warmup_interval_secs` while idle so
    /// LMCache keeps hot KV state for the common flow steps.
    #[serde(default)]
    pub warmup_prefixes: Vec<WarmupPrefix>,

    /// Seconds between warmup-loop ticks.
    #[serde(default = "default_warmup_interval_secs")]
    pub warmup_interval_secs: u64,

    /// Dream flows — executed during idle windows beyond the warmup pass.
    #[serde(default)]
    pub dream_flows: Vec<DreamFlow>,

    /// Sprint tag applied to persisted dream engrams.
    #[serde(default = "default_sprint_tag")]
    pub sprint_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmupPrefix {
    /// Human-readable label, used in logs and metric labels.
    pub name: String,
    /// Backend model id (e.g. "gemma4:e4b" in the llama-swap routing pool).
    pub model: String,
    /// Target URL — typically the llama-swap endpoint on Hugin.
    pub url: String,
    /// Shared system prompt — match Odin's SWARM_SHARED_SYSTEM exactly so
    /// the LMCache prefix-hit actually serves real Odin traffic.
    pub system: String,
    /// User-message prefix seed.
    pub user_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamFlow {
    pub name: String,
    /// Optional cron schedule (5 or 6 field). If present, fires on schedule.
    /// If absent, fires whenever idle_duration > min_idle_secs.
    pub cron: Option<String>,
    /// Prompt the dreamer sends to the flow endpoint.
    pub prompt: String,
    /// Flow name to invoke via Odin (e.g. "dream_exploration").
    pub flow: String,
}

fn default_min_idle_secs() -> u64 {
    300
}
fn default_poll_interval_secs() -> u64 {
    30
}
fn default_listen_addr() -> String {
    "0.0.0.0:9097".to_string()
}
fn default_warmup_interval_secs() -> u64 {
    600
}
fn default_sprint_tag() -> String {
    "sprint:065".to_string()
}

impl DreamerConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        let cfg: Self = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let json = r#"{
            "odin_url": "http://10.0.65.8:8080",
            "mimir_url": "http://10.0.65.8:9090"
        }"#;
        let cfg: DreamerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.odin_url, "http://10.0.65.8:8080");
        assert_eq!(cfg.min_idle_secs, 300);
        assert_eq!(cfg.listen_addr, "0.0.0.0:9097");
        assert_eq!(cfg.sprint_tag, "sprint:065");
        assert!(cfg.warmup_prefixes.is_empty());
        assert!(cfg.dream_flows.is_empty());
    }

    #[test]
    fn parses_full_config_with_warmup() {
        let json = r#"{
            "odin_url": "http://10.0.65.8:8080",
            "mimir_url": "http://10.0.65.8:9090",
            "min_idle_secs": 600,
            "warmup_prefixes": [
                {
                    "name": "coding_swarm_drafter",
                    "model": "gemma4:e4b",
                    "url": "http://10.0.65.9:11500",
                    "system": "You are a code drafter.",
                    "user_prefix": "Draft a function that..."
                }
            ]
        }"#;
        let cfg: DreamerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.min_idle_secs, 600);
        assert_eq!(cfg.warmup_prefixes.len(), 1);
        assert_eq!(cfg.warmup_prefixes[0].name, "coding_swarm_drafter");
    }
}
