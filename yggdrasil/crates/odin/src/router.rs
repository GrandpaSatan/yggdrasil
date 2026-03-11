/// Rule-based semantic router.
///
/// Classifies the user's message into one of three intents (coding, reasoning,
/// home_assistant) using keyword matching, then maps the intent to a model and
/// backend URL.  This is a v1 keyword router — Sprint 006+ will upgrade to
/// embedding-based classification.
///
/// Design decisions (from sprint 005 Decision Log):
/// - Keyword sets are hardcoded, not configurable, because they are an
///   implementation detail of the routing heuristic.
/// - The rule with the highest keyword match count wins; ties break on rule
///   order (first defined wins).
/// - If no rule matches any keyword the default model/backend is used.
use std::collections::HashMap;

use ygg_domain::config::{BackendConfig, BackendType, RoutingConfig};

// ─────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────

/// The result of classifying a user message.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub intent: String,
    pub model: String,
    pub backend_url: String,
    pub backend_name: String,
    pub backend_type: BackendType,
}

// ─────────────────────────────────────────────────────────────────
// Internal types
// ─────────────────────────────────────────────────────────────────

/// A compiled routing rule with its resolved keyword set and backend URL.
struct CompiledRule {
    intent: String,
    keywords: Vec<String>,
    model: String,
    backend_name: String,
    backend_url: String,
    backend_type: BackendType,
}

// ─────────────────────────────────────────────────────────────────
// Keyword sets (hardcoded per intent)
// ─────────────────────────────────────────────────────────────────

fn coding_keywords() -> Vec<String> {
    [
        "implement", "function", "code", "bug", "error", "compile", "test",
        "refactor", "debug", "syntax", "struct", "enum", "trait", "fn",
        "class", "method", "variable", "import", "module", "crate", "cargo",
        "rustc", "clippy", "lint", "type",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn reasoning_keywords() -> Vec<String> {
    [
        "explain", "why", "how", "analyze", "design", "architecture", "plan",
        "compare", "evaluate", "reason", "think", "consider", "strategy",
        "tradeoff", "trade-off", "pros", "cons", "overview",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn ha_keywords() -> Vec<String> {
    [
        "home assistant",
        "home automation",
        "smart home",
        "iot",
        "automation",
        "light",
        "switch",
        "sensor",
        "entity",
        "hass",
        "ha ",
        "thermostat",
        "scene",
        "script",
        "trigger",
        "action",
        "climate",
        "cover",
        "fan",
        "lock",
        "alarm",
        "binary_sensor",
        "media_player",
        "camera",
        "vacuum",
        "garage",
        "door",
        "temperature",
        "humidity",
        "motion",
        "occupancy",
        "energy",
        "power",
        "battery",
        "turn on",
        "turn off",
        "toggle",
        "brightness",
        "color",
        "heating",
        "cooling",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn keywords_for_intent(intent: &str) -> Vec<String> {
    match intent {
        "coding" => coding_keywords(),
        "reasoning" => reasoning_keywords(),
        // Both "home_assistant" (legacy) and "home_automation" (Sprint 007 spec)
        // share the same keyword set so either config name works.
        "home_assistant" | "home_automation" => ha_keywords(),
        // Default/general are catch-all intents with no keywords.
        "default" | "general" => vec![],
        other => {
            tracing::warn!(intent = other, "unknown intent in routing config — no keywords assigned");
            vec![]
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// SemanticRouter
// ─────────────────────────────────────────────────────────────────

/// Keyword-based semantic router.
///
/// Constructed once during startup from the `OdinConfig` and shared across
/// all request handlers via `AppState`.  The type is `Clone` because Axum
/// requires shared state to be `Clone`.
#[derive(Clone)]
pub struct SemanticRouter {
    rules: Vec<CompiledRuleOwned>,
    /// model name -> backend name
    model_to_backend: HashMap<String, String>,
    /// backend name -> url
    backend_urls: HashMap<String, String>,
    /// backend name -> type
    backend_types: HashMap<String, BackendType>,
    default_model: String,
    default_backend_name: String,
    default_backend_url: String,
    default_backend_type: BackendType,
}

/// Owned version of `CompiledRule` that is `Clone`.
#[derive(Clone)]
struct CompiledRuleOwned {
    intent: String,
    keywords: Vec<String>,
    model: String,
    backend_name: String,
    backend_url: String,
    backend_type: BackendType,
}

impl From<CompiledRule> for CompiledRuleOwned {
    fn from(r: CompiledRule) -> Self {
        Self {
            intent: r.intent,
            keywords: r.keywords,
            model: r.model,
            backend_name: r.backend_name,
            backend_url: r.backend_url,
            backend_type: r.backend_type,
        }
    }
}

impl SemanticRouter {
    /// Build the router from the YAML-derived config structs.
    ///
    /// Rules that reference an unknown backend name are skipped with a warning.
    /// If the default model's backend cannot be resolved, the first configured
    /// backend is used as the fallback.
    #[must_use]
    pub fn new(config: &RoutingConfig, backends: &[BackendConfig]) -> Self {
        // Build lookup maps from the backend list.
        let backend_urls: HashMap<String, String> = backends
            .iter()
            .map(|b| (b.name.clone(), b.url.clone()))
            .collect();

        let backend_types: HashMap<String, BackendType> = backends
            .iter()
            .map(|b| (b.name.clone(), b.backend_type.clone()))
            .collect();

        // Map every model name to the backend that lists it.
        let model_to_backend: HashMap<String, String> = backends
            .iter()
            .flat_map(|b| b.models.iter().map(|m| (m.clone(), b.name.clone())))
            .collect();

        // Compile routing rules.
        let rules: Vec<CompiledRuleOwned> = config
            .rules
            .iter()
            .filter_map(|rule| {
                match backend_urls.get(&rule.backend) {
                    Some(url) => {
                        let bt = backend_types
                            .get(&rule.backend)
                            .cloned()
                            .unwrap_or_default();
                        Some(
                            CompiledRule {
                                intent: rule.intent.clone(),
                                keywords: keywords_for_intent(&rule.intent),
                                model: rule.model.clone(),
                                backend_name: rule.backend.clone(),
                                backend_url: url.clone(),
                                backend_type: bt,
                            }
                            .into(),
                        )
                    }
                    None => {
                        tracing::warn!(
                            intent = %rule.intent,
                            backend = %rule.backend,
                            "routing rule references unknown backend — rule skipped"
                        );
                        None
                    }
                }
            })
            .collect();

        // Resolve the default backend.
        // Prefer explicit `default_backend` config, then fall back to model-based resolution.
        let (default_backend_name, default_backend_url, default_backend_type) = config
            .default_backend
            .as_ref()
            .and_then(|name| {
                backend_urls.get(name).map(|url| {
                    let bt = backend_types.get(name).cloned().unwrap_or_default();
                    (name.clone(), url.clone(), bt)
                })
            })
            .or_else(|| {
                model_to_backend
                    .get(&config.default_model)
                    .and_then(|name| {
                        backend_urls.get(name).map(|url| {
                            let bt = backend_types.get(name).cloned().unwrap_or_default();
                            (name.clone(), url.clone(), bt)
                        })
                    })
            })
            .or_else(|| {
                backends
                    .first()
                    .map(|b| (b.name.clone(), b.url.clone(), b.backend_type.clone()))
            })
            .unwrap_or_else(|| {
                tracing::warn!("no backends configured — routing will fail at runtime");
                (String::new(), String::new(), BackendType::default())
            });

        Self {
            rules,
            model_to_backend,
            backend_urls,
            backend_types,
            default_model: config.default_model.clone(),
            default_backend_name,
            default_backend_url,
            default_backend_type,
        }
    }

    /// Classify a user message and return a `RoutingDecision`.
    ///
    /// The message is lowercased and each compiled rule's keywords are checked
    /// for substring presence.  The rule with the highest match count wins.
    /// Ties are broken by rule order (first rule defined wins).  If no rule
    /// matches, the default model/backend is returned.
    #[must_use]
    pub fn classify(&self, message: &str) -> RoutingDecision {
        let lower = message.to_lowercase();

        let best = self
            .rules
            .iter()
            .map(|rule| {
                let count = rule
                    .keywords
                    .iter()
                    .filter(|kw| lower.contains(kw.as_str()))
                    .count();
                (count, rule)
            })
            .filter(|(count, _)| *count > 0)
            .max_by_key(|(count, _)| *count);

        match best {
            Some((_, rule)) => {
                tracing::debug!(
                    intent = %rule.intent,
                    model = %rule.model,
                    backend = %rule.backend_name,
                    "routing decision made by keyword match"
                );
                RoutingDecision {
                    intent: rule.intent.clone(),
                    model: rule.model.clone(),
                    backend_url: rule.backend_url.clone(),
                    backend_name: rule.backend_name.clone(),
                    backend_type: rule.backend_type.clone(),
                }
            }
            None => {
                tracing::debug!(
                    model = %self.default_model,
                    backend = %self.default_backend_name,
                    "no keyword match — using default backend"
                );
                RoutingDecision {
                    intent: "default".to_string(),
                    model: self.default_model.clone(),
                    backend_url: self.default_backend_url.clone(),
                    backend_name: self.default_backend_name.clone(),
                    backend_type: self.default_backend_type.clone(),
                }
            }
        }
    }

    /// Resolve the backend URL and name for an explicitly requested model.
    ///
    /// Returns `None` if the model is not listed by any configured backend.
    #[must_use]
    pub fn resolve_backend_for_model(&self, model: &str) -> Option<RoutingDecision> {
        let backend_name = self.model_to_backend.get(model)?;
        let backend_url = self.backend_urls.get(backend_name)?;
        let backend_type = self
            .backend_types
            .get(backend_name)
            .cloned()
            .unwrap_or_default();
        Some(RoutingDecision {
            intent: "explicit".to_string(),
            model: model.to_string(),
            backend_url: backend_url.clone(),
            backend_name: backend_name.clone(),
            backend_type,
        })
    }
}
