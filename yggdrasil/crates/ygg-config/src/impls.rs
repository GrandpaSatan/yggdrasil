//! Validate implementations for Yggdrasil config structs.
//!
//! Provides concrete [`Validate`] implementations for the domain config types
//! so callers can call `config.validate()` after loading.

use ygg_domain::config::{
    HaConfig, HuginnConfig, MimirConfig, MuninnConfig, OdinConfig,
};

use crate::validate::{
    Validate, ValidationError, validate_listen_addr, validate_no_unexpanded_vars,
    validate_not_empty, validate_url,
};

impl Validate for OdinConfig {
    fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if let Some(e) = validate_listen_addr("listen_addr", &self.listen_addr) {
            errors.push(e);
        }
        if let Some(e) = validate_not_empty("node_name", &self.node_name) {
            errors.push(e);
        }
        if self.backends.is_empty() {
            errors.push(ValidationError::new("backends", "must have at least one backend"));
        }
        for (i, backend) in self.backends.iter().enumerate() {
            let prefix = format!("backends[{i}]");
            if let Some(e) = validate_not_empty(&format!("{prefix}.name"), &backend.name) {
                errors.push(e);
            }
            if let Some(e) = validate_url(&format!("{prefix}.url"), &backend.url) {
                errors.push(e);
            }
        }
        if let Some(e) = validate_url("mimir.url", &self.mimir.url) {
            errors.push(e);
        }
        if let Some(e) = validate_url("muninn.url", &self.muninn.url) {
            errors.push(e);
        }
        if let Some(ref ha) = self.ha {
            errors.extend(ha.validate());
        }
        // Check cloud provider API keys for unexpanded env vars
        if let Some(ref cloud) = self.cloud {
            if let Some(ref openai) = cloud.openai
                && let Some(e) = validate_no_unexpanded_vars("cloud.openai.api_key", &openai.api_key)
                {
                    errors.push(e);
                }
            if let Some(ref claude) = cloud.claude
                && let Some(e) =
                    validate_no_unexpanded_vars("cloud.claude.api_key", &claude.api_key)
                {
                    errors.push(e);
                }
            if let Some(ref gemini) = cloud.gemini
                && let Some(e) =
                    validate_no_unexpanded_vars("cloud.gemini.api_key", &gemini.api_key)
                {
                    errors.push(e);
                }
        }

        errors
    }
}

impl Validate for HaConfig {
    fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if let Some(e) = validate_url("ha.url", &self.url) {
            errors.push(e);
        }
        if let Some(e) = validate_not_empty("ha.token", &self.token) {
            errors.push(e);
        }
        if let Some(e) = validate_no_unexpanded_vars("ha.token", &self.token) {
            errors.push(e);
        }

        errors
    }
}

impl Validate for MimirConfig {
    fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if let Some(e) = validate_listen_addr("listen_addr", &self.listen_addr) {
            errors.push(e);
        }
        if let Some(e) = validate_not_empty("database_url", &self.database_url) {
            errors.push(e);
        }
        if let Some(e) = validate_no_unexpanded_vars("database_url", &self.database_url) {
            errors.push(e);
        }
        if let Some(e) = validate_url("qdrant_url", &self.qdrant_url) {
            errors.push(e);
        }
        if let Some(e) = validate_not_empty("sdr.model_dir", &self.sdr.model_dir) {
            errors.push(e);
        }

        errors
    }
}

impl Validate for HuginnConfig {
    fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if let Some(e) = validate_listen_addr("listen_addr", &self.listen_addr) {
            errors.push(e);
        }
        if let Some(e) = validate_not_empty("database_url", &self.database_url) {
            errors.push(e);
        }
        if let Some(e) = validate_url("qdrant_url", &self.qdrant_url) {
            errors.push(e);
        }
        if self.watch_paths.is_empty() {
            errors.push(ValidationError::new("watch_paths", "must have at least one path"));
        }

        errors
    }
}

impl Validate for MuninnConfig {
    fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if let Some(e) = validate_listen_addr("listen_addr", &self.listen_addr) {
            errors.push(e);
        }
        if let Some(e) = validate_not_empty("database_url", &self.database_url) {
            errors.push(e);
        }
        if let Some(e) = validate_url("qdrant_url", &self.qdrant_url) {
            errors.push(e);
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ygg_domain::config::*;

    fn minimal_odin_config() -> OdinConfig {
        OdinConfig {
            node_name: "test".to_string(),
            listen_addr: "0.0.0.0:8080".to_string(),
            backends: vec![BackendConfig {
                name: "local".to_string(),
                url: "http://localhost:11434".to_string(),
                backend_type: BackendType::default(),
                models: vec!["test".to_string()],
                max_concurrent: 2,
                context_window: 16384,
            }],
            routing: RoutingConfig {
                default_model: "test".to_string(),
                default_backend: None,
                rules: vec![],
            },
            mimir: MimirClientConfig {
                url: "http://localhost:9090".to_string(),
                query_limit: 5,
                store_on_completion: true,
            },
            muninn: MuninnClientConfig {
                url: "http://localhost:9091".to_string(),
                max_context_chunks: 10,
            },
            ha: None,
            session: SessionConfig::default(),
            cloud: None,
            voice: None,
            agent: None,
            task_worker: None,
            web_search: None,
        }
    }

    #[test]
    fn valid_odin_config_passes() {
        let config = minimal_odin_config();
        assert!(config.validate().is_empty());
    }

    #[test]
    fn odin_config_invalid_listen_addr() {
        let mut config = minimal_odin_config();
        config.listen_addr = "bad-addr".to_string();
        let errors = config.validate();
        assert!(!errors.is_empty());
        assert!(errors[0].field == "listen_addr");
    }

    #[test]
    fn odin_config_empty_node_name() {
        let mut config = minimal_odin_config();
        config.node_name = "".to_string();
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.field == "node_name"));
    }

    #[test]
    fn odin_config_unexpanded_cloud_key() {
        let mut config = minimal_odin_config();
        config.cloud = Some(CloudProvidersConfig {
            fallback_enabled: true,
            openai: Some(CloudProviderEntry {
                api_key: "${OPENAI_API_KEY}".to_string(),
                default_model: "gpt-4o-mini".to_string(),
                requests_per_minute: 60,
            }),
            claude: None,
            gemini: None,
        });
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.field.contains("openai.api_key")));
    }

    #[test]
    fn ha_config_invalid_url() {
        let ha = HaConfig {
            url: "not-a-url".to_string(),
            token: "valid-token".to_string(),
            timeout_secs: 10,
        };
        let errors = ha.validate();
        assert!(errors.iter().any(|e| e.field.contains("url")));
    }

    #[test]
    fn mimir_config_validation() {
        let config = MimirConfig {
            listen_addr: "0.0.0.0:9090".to_string(),
            database_url: "postgres://localhost/test".to_string(),
            qdrant_url: "http://localhost:6333".to_string(),
            sdr: SdrConfig {
                dim_bits: 256,
                model_dir: "/opt/models".to_string(),
                dedup_threshold: 0.85,
            },
            tiers: TierConfig {
                recall_capacity: 1000,
                summarization_batch_size: 100,
                check_interval_secs: 300,
                min_age_secs: 86400,
                odin_url: "http://localhost:8080".to_string(),
            },
            auto_ingest: None,
        };
        assert!(config.validate().is_empty());
    }
}
