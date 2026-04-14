/// Keyword-based semantic router (fallback layer).
///
/// Classifies the user's message into one of three intents (coding, reasoning,
/// home_automation) using keyword matching, then maps the intent to a model and
/// backend URL.
///
/// Since Sprint 052, this is the **fallback** layer.  Primary routing goes
/// through the hybrid SDR + LLM pipeline (`sdr_router` → `llm_router`).
/// The keyword router activates only when both the SDR and LLM routers are
/// unavailable or low-confidence.
///
/// Design decisions:
/// - The rule with the highest keyword match count wins; ties break on rule
///   order (first defined wins).
/// - If no rule matches any keyword the default model/backend is used.
use std::collections::HashMap;

use serde::Serialize;
use ygg_domain::config::{BackendConfig, BackendType, RoutingConfig};

// ─────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────

/// How the routing decision was made (Sprint 052).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouterMethod {
    /// Existing keyword match (pre-Sprint 052 behaviour).
    Keyword,
    /// SDR prototype was confident and LLM was unavailable.
    SdrOnly,
    /// LLM confirmed the SDR suggestion.
    LlmConfirmed,
    /// LLM disagreed with SDR and overrode the intent.
    LlmOverride,
    /// Client specified the model explicitly.
    Explicit,
    /// Sprint 063 P1: client specified a flow via `flow` param — dispatched
    /// directly without intent classification. Distinct from `Explicit` so
    /// observability can tell model-pin apart from flow-pin.
    ExplicitFlow,
    /// All routers failed — used keyword fallback.
    Fallback,
}

/// How the keyword classifier arrived at its decision (Sprint 062 P1a).
///
/// Disambiguates the three distinct origins of an `intent="default"` result so
/// that downstream fallback logic in `handlers.rs` can tell a real-but-weak
/// match (`Matched`) apart from a gaming-suppressed HA match (`Suppressed`)
/// and a genuine no-match (`None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordMatchKind {
    /// No rule matched any keyword in the message.
    None,
    /// A rule matched (HA) but was suppressed by a gaming keyword co-occurrence.
    Suppressed,
    /// At least one rule matched — see `keyword_match_count` for strength.
    Matched,
}

/// The result of classifying a user message.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub intent: String,
    /// Confidence from SDR or LLM classification. `None` for keyword/explicit.
    pub confidence: Option<f64>,
    /// Which routing method produced this decision.
    pub router_method: RouterMethod,
    pub model: String,
    pub backend_url: String,
    pub backend_name: String,
    pub backend_type: BackendType,
    /// Number of keyword hits that produced this decision (Sprint 062 P1a).
    /// 0 for suppressed / no-match / non-keyword paths.
    pub keyword_match_count: u32,
    /// Origin category of the keyword classifier verdict (Sprint 062 P1a).
    pub keyword_match_kind: KeywordMatchKind,
    /// Sprint 063 P1: name of the explicitly pinned flow, when the caller set
    /// `ChatCompletionRequest.flow`. `None` on every classification-driven path.
    /// `Some(name)` only when `router_method == RouterMethod::ExplicitFlow`.
    pub explicit_flow: Option<String>,
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

/// Keywords that indicate a gaming/VM/inference request.
/// Used to suppress HA intent when these terms co-occur with "turn on".
fn gaming_keywords() -> Vec<String> {
    [
        "thor", "plume", "harpy", "morrigan", "nightjar", "chirp",
        "gaming vm", "launch vm", "start vm", "load harpy", "load morrigan",
        "code locally", "local coding", "local inference", "local llm",
        "inference vm", "start nightjar", "start chirp",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn research_keywords() -> Vec<String> {
    [
        "research", "investigate", "deep dive", "comprehensive analysis",
        "literature review", "survey", "compare approaches", "state of the art",
        "what are the options", "explore alternatives", "find out",
        "summarize findings", "gather information", "look into",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn keywords_for_intent(intent: &str) -> Vec<String> {
    match intent {
        "coding" => coding_keywords(),
        "reasoning" => reasoning_keywords(),
        "research" => research_keywords(),
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

        // Gaming override: if HA intent won but gaming keywords are present,
        // suppress to default so HA context doesn't mislead the LLM into
        // treating Thor/Harpy as Home Assistant entities.
        let gaming_kws = gaming_keywords();
        let has_gaming = gaming_kws.iter().any(|kw| lower.contains(kw.as_str()));

        // Sprint 062 P1a: track whether suppression fired so downstream
        // fallback logic can distinguish "no match" from "HA match was
        // intentionally downgraded" from "real match".
        let (best, suppressed) = match best {
            Some((_, rule))
                if (rule.intent == "home_automation" || rule.intent == "home_assistant")
                    && has_gaming =>
            {
                tracing::info!("gaming keyword detected — suppressing HA intent to default");
                (None, true)
            }
            other => (other, false),
        };

        match best {
            Some((count, rule)) => {
                tracing::debug!(
                    intent = %rule.intent,
                    model = %rule.model,
                    backend = %rule.backend_name,
                    keyword_match_count = count,
                    "routing decision made by keyword match"
                );
                RoutingDecision {
                    intent: rule.intent.clone(),
                    confidence: None,
                    router_method: RouterMethod::Keyword,
                    model: rule.model.clone(),
                    backend_url: rule.backend_url.clone(),
                    backend_name: rule.backend_name.clone(),
                    backend_type: rule.backend_type.clone(),
                    keyword_match_count: count as u32,
                    keyword_match_kind: KeywordMatchKind::Matched,
                    explicit_flow: None,
                }
            }
            None => {
                let kind = if suppressed {
                    KeywordMatchKind::Suppressed
                } else {
                    KeywordMatchKind::None
                };
                tracing::debug!(
                    model = %self.default_model,
                    backend = %self.default_backend_name,
                    keyword_match_kind = ?kind,
                    "no keyword match — using default backend"
                );
                RoutingDecision {
                    intent: "default".to_string(),
                    confidence: None,
                    router_method: RouterMethod::Keyword,
                    model: self.default_model.clone(),
                    backend_url: self.default_backend_url.clone(),
                    backend_name: self.default_backend_name.clone(),
                    backend_type: self.default_backend_type.clone(),
                    keyword_match_count: 0,
                    keyword_match_kind: kind,
                    explicit_flow: None,
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
            confidence: None,
            router_method: RouterMethod::Explicit,
            model: model.to_string(),
            backend_url: backend_url.clone(),
            backend_name: backend_name.clone(),
            backend_type,
            keyword_match_count: 0,
            keyword_match_kind: KeywordMatchKind::None,
            explicit_flow: None,
        })
    }

    /// Resolve a `RoutingDecision` for a given intent string (Sprint 052).
    ///
    /// Used by the hybrid SDR+LLM router to map an LLM-classified intent name
    /// back to a model/backend pair from the routing rules.
    /// Returns `None` if no rule matches the intent.
    #[must_use]
    pub fn resolve_intent(&self, intent: &str) -> Option<RoutingDecision> {
        self.rules
            .iter()
            .find(|r| r.intent == intent)
            .map(|rule| RoutingDecision {
                intent: rule.intent.clone(),
                confidence: None,
                router_method: RouterMethod::Keyword, // caller overrides
                model: rule.model.clone(),
                backend_url: rule.backend_url.clone(),
                backend_name: rule.backend_name.clone(),
                backend_type: rule.backend_type.clone(),
                keyword_match_count: 0,
                keyword_match_kind: KeywordMatchKind::None,
                explicit_flow: None,
            })
    }

    /// Sprint 063 P1: build a `RoutingDecision` that pins execution to a named
    /// flow without running intent classification.
    ///
    /// The resulting decision uses `RouterMethod::ExplicitFlow` with
    /// `explicit_flow = Some(flow_name)`, intent = "explicit_flow", and
    /// default model/backend (the flow itself drives actual model selection
    /// per-step). This is a bookkeeping decision — `handlers.rs` dispatches
    /// via `flow_engine.find_by_name()` before touching the model/backend
    /// fields.
    #[must_use]
    pub fn explicit_flow_decision(&self, flow_name: &str) -> RoutingDecision {
        RoutingDecision {
            intent: "explicit_flow".to_string(),
            confidence: None,
            router_method: RouterMethod::ExplicitFlow,
            model: self.default_model.clone(),
            backend_url: self.default_backend_url.clone(),
            backend_name: self.default_backend_name.clone(),
            backend_type: self.default_backend_type.clone(),
            keyword_match_count: 0,
            keyword_match_kind: KeywordMatchKind::None,
            explicit_flow: Some(flow_name.to_string()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests (Sprint 062 P1a — intent_default keyword-classifier signal)
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ygg_domain::config::{BackendConfig, BackendType, RoutingConfig, RoutingRule};

    /// Build a `SemanticRouter` with the three intents used in Sprint 062 tests:
    /// `home_automation`, `coding`, and `reasoning`. Default backend is `chat`.
    fn build_test_router() -> SemanticRouter {
        let backends = vec![
            BackendConfig {
                name: "ha-backend".to_string(),
                url: "http://ha.local:11434".to_string(),
                backend_type: BackendType::Ollama,
                models: vec!["ha-model".to_string()],
                max_concurrent: 2,
                context_window: 16384,
            },
            BackendConfig {
                name: "coder-backend".to_string(),
                url: "http://coder.local:11434".to_string(),
                backend_type: BackendType::Ollama,
                models: vec!["coder-model".to_string()],
                max_concurrent: 2,
                context_window: 16384,
            },
            BackendConfig {
                name: "default-backend".to_string(),
                url: "http://default.local:11434".to_string(),
                backend_type: BackendType::Ollama,
                models: vec!["default-model".to_string()],
                max_concurrent: 2,
                context_window: 16384,
            },
        ];
        let config = RoutingConfig {
            default_model: "default-model".to_string(),
            default_backend: Some("default-backend".to_string()),
            rules: vec![
                RoutingRule {
                    intent: "home_automation".to_string(),
                    model: "ha-model".to_string(),
                    backend: "ha-backend".to_string(),
                },
                RoutingRule {
                    intent: "coding".to_string(),
                    model: "coder-model".to_string(),
                    backend: "coder-backend".to_string(),
                },
            ],
            intent_default: Some("chat".to_string()),
        };
        SemanticRouter::new(&config, &backends)
    }

    #[test]
    fn test_ha_high_confidence_beats_default() {
        // "turn on the kitchen light" contains at least two HA keywords
        // ("turn on" and "light") so the classifier must produce a strong
        // Matched decision that downstream fallback logic will respect.
        let router = build_test_router();
        let decision = router.classify("turn on the kitchen light");
        assert_eq!(decision.intent, "home_automation");
        assert_eq!(decision.keyword_match_kind, KeywordMatchKind::Matched);
        assert!(
            decision.keyword_match_count >= 2,
            "expected at least 2 keyword hits, got {}",
            decision.keyword_match_count
        );
    }

    #[test]
    fn test_gaming_suppression_still_works() {
        // "harpy is being a dick in fallout" contains the gaming keyword
        // "harpy" — if any HA keyword also matched, the gaming override
        // must suppress it. Otherwise the kind is None. Either way, the
        // fallback arm in handlers.rs must NOT override into chat because
        // a Suppressed decision is an intentional pass-through for the LLM.
        let router = build_test_router();
        let decision = router.classify("harpy is being a dick in fallout");
        assert_eq!(decision.intent, "default");
        // The important invariant: this must NOT come back as Matched.
        assert_ne!(decision.keyword_match_kind, KeywordMatchKind::Matched);
        assert_eq!(decision.keyword_match_count, 0);
    }

    #[test]
    fn test_no_match_applies_intent_default() {
        // A bare greeting matches no rule at all — kind is None, count is 0.
        // This is the ONLY shape that permits the `intent_default` override
        // in handlers.rs to fire.
        let router = build_test_router();
        let decision = router.classify("hi");
        assert_eq!(decision.intent, "default");
        assert_eq!(decision.keyword_match_kind, KeywordMatchKind::None);
        assert_eq!(decision.keyword_match_count, 0);
    }

    #[test]
    fn test_ambiguous_ha_with_gaming_mention() {
        // "turn on kitchen light while I play Fallout" — HA keywords
        // ("turn on", "light") win. "Fallout" is NOT in the gaming suppression
        // set (only Thor/Harpy/Morrigan/etc are), so HA stays.
        let router = build_test_router();
        let decision = router.classify("turn on kitchen light while I play Fallout");
        assert_eq!(decision.intent, "home_automation");
        assert_eq!(decision.keyword_match_kind, KeywordMatchKind::Matched);
        assert!(
            decision.keyword_match_count >= 2,
            "expected HA keywords to win with count >= 2, got {}",
            decision.keyword_match_count
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Sprint 063 P5d — Router adversarial regression suite
    //
    // Each case below either:
    //   (a) asserts a specific intent/keyword_match_kind, OR
    //   (b) asserts that classify() returns a valid decision without
    //       panicking / allocating unreasonably.
    // None may crash. The default model/backend are always a safe fallback.
    // ─────────────────────────────────────────────────────────────

    /// P5d-01: classic HA query must remain HA across config iterations.
    #[test]
    fn p5d_ha_plain_turn_on_lights() {
        let router = build_test_router();
        let d = router.classify("turn on the lights");
        assert_eq!(d.intent, "home_automation");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::Matched);
    }

    /// P5d-02: HA + gaming node name suppresses to default (Sprint 062 P1a).
    #[test]
    fn p5d_ha_plus_gaming_vm_suppressed() {
        let router = build_test_router();
        let d = router.classify("turn on thor and launch the gaming VM");
        assert_eq!(d.intent, "default");
        assert_ne!(d.keyword_match_kind, KeywordMatchKind::Matched);
    }

    /// P5d-03: HA + coding ask — the coding verb "implement" plus "kitchen
    /// light" should resolve to the higher-match intent. "light" counts as
    /// HA but "implement" is coding, so the classifier picks whichever rule
    /// hit more keywords. The invariant is merely: we don't crash and we
    /// don't return an intent we never declared.
    #[test]
    fn p5d_ha_plus_coding_implement_kitchen_light_handler() {
        let router = build_test_router();
        let d = router.classify("implement the kitchen light handler");
        assert!(["home_automation", "coding", "default"].contains(&d.intent.as_str()));
        assert!(!d.backend_url.is_empty(), "must pick a concrete backend");
    }

    /// P5d-04: "turn on the debugger" — ambiguous tool word. "turn on"
    /// is HA; "debugger" is coding-flavored. Don't crash; return SOME
    /// decision with a non-empty backend.
    #[test]
    fn p5d_ambiguous_turn_on_the_debugger() {
        let router = build_test_router();
        let d = router.classify("turn on the debugger");
        assert!(!d.backend_url.is_empty());
        assert!(!d.model.is_empty());
    }

    /// P5d-05: empty input — must not panic, returns default with
    /// `keyword_match_kind == None`.
    #[test]
    fn p5d_empty_input() {
        let router = build_test_router();
        let d = router.classify("");
        assert_eq!(d.intent, "default");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::None);
        assert_eq!(d.keyword_match_count, 0);
    }

    /// P5d-06: whitespace-only input.
    #[test]
    fn p5d_whitespace_only_input() {
        let router = build_test_router();
        let d = router.classify("   \n\t   ");
        assert_eq!(d.intent, "default");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::None);
    }

    /// P5d-07: unicode + emoji input — classifier must not panic on
    /// char-boundary arithmetic.
    #[test]
    fn p5d_unicode_and_emoji() {
        let router = build_test_router();
        let d = router.classify("🏠 turn on the 灯 light please 🚀💡");
        // HA keywords ("turn on", "light") should still match.
        assert_eq!(d.intent, "home_automation");
    }

    /// P5d-08: mixed-case doesn't break lowercasing.
    #[test]
    fn p5d_mixed_case_turn_on() {
        let router = build_test_router();
        let d = router.classify("TURN ON the Kitchen LIGHT");
        assert_eq!(d.intent, "home_automation");
    }

    /// P5d-09: single-word input "lights" — matches HA even without a verb.
    #[test]
    fn p5d_single_word_lights() {
        let router = build_test_router();
        let d = router.classify("lights");
        // "lights" contains "light" substring → HA match.
        assert_eq!(d.intent, "home_automation");
    }

    /// P5d-10: single bare verb "implement".
    #[test]
    fn p5d_single_word_implement() {
        let router = build_test_router();
        let d = router.classify("implement");
        assert_eq!(d.intent, "coding");
    }

    /// P5d-11: multi-intent mash — "turn off lights and write code".
    /// The classifier picks the rule with the highest count; ties break
    /// on rule order. Must be a declared intent.
    #[test]
    fn p5d_multi_intent_lights_and_code() {
        let router = build_test_router();
        let d = router.classify("turn off the lights and write some code");
        assert!(
            ["home_automation", "coding"].contains(&d.intent.as_str()),
            "got unexpected intent {}",
            d.intent
        );
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::Matched);
    }

    /// P5d-12: SQL-injection attempt — treated as plain text.
    #[test]
    fn p5d_sql_injection_attempt() {
        let router = build_test_router();
        let d = router.classify("'; DROP TABLE engrams; -- turn on the light");
        // "turn on" + "light" still match HA; the SQL is meaningless here.
        assert_eq!(d.intent, "home_automation");
    }

    /// P5d-13: 10KB input — must not OOM or time out.
    #[test]
    fn p5d_10kb_input() {
        let router = build_test_router();
        let big = "turn on the light ".repeat(600); // ~10.8 KB
        assert!(big.len() >= 10_000);
        let d = router.classify(&big);
        assert_eq!(d.intent, "home_automation");
        assert!(d.keyword_match_count > 0);
    }

    /// P5d-14: purely numeric input.
    #[test]
    fn p5d_purely_numeric() {
        let router = build_test_router();
        let d = router.classify("12345 67890 3.14159");
        assert_eq!(d.intent, "default");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::None);
    }

    /// P5d-15: purely punctuation.
    #[test]
    fn p5d_purely_punctuation() {
        let router = build_test_router();
        let d = router.classify("!@#$%^&*()-=_+[]{}|;:',.<>?/`~");
        assert_eq!(d.intent, "default");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::None);
    }

    /// P5d-16: HTML-looking input falls through gracefully. The
    /// classifier is substring-based so tag names that happen to overlap
    /// real keywords (e.g. `<script>` matches the HA `script` keyword,
    /// `class="..."` matches the coding `class` keyword) are expected —
    /// the invariant we enforce is that the classifier produces a
    /// non-crashing decision with a valid backend regardless.
    #[test]
    fn p5d_html_looking_input() {
        let router = build_test_router();
        let d = router.classify("<span>hello</span><br/>");
        // No HA/coding/reasoning keywords hit here → default.
        assert_eq!(d.intent, "default");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::None);
    }

    /// P5d-17: gaming keyword alone with no HA verbs — default.
    #[test]
    fn p5d_gaming_node_alone() {
        let router = build_test_router();
        let d = router.classify("how's Thor doing today?");
        // "thor" is a gaming keyword, but no HA keywords match, so there's
        // nothing to suppress. Falls through to default via None path.
        assert_eq!(d.intent, "default");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::None);
    }

    /// P5d-18: gaming keyword with HA verb — Suppressed kind.
    #[test]
    fn p5d_gaming_plus_ha_verb_is_suppressed() {
        let router = build_test_router();
        let d = router.classify("turn on thor for gaming");
        assert_eq!(d.intent, "default");
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::Suppressed);
        assert_eq!(d.keyword_match_count, 0);
    }

    /// P5d-19: every configured backend has a non-empty URL. A classify
    /// result with an empty backend_url would break dispatch downstream.
    #[test]
    fn p5d_classify_never_returns_empty_backend_url() {
        let router = build_test_router();
        for input in [
            "",
            "turn on the lights",
            "implement a function",
            "explain how it works",
            "thor launch harpy",
            "\u{0}",                // NUL
            "a".repeat(20_000).as_str(),
        ] {
            let d = router.classify(input);
            assert!(!d.backend_url.is_empty(), "empty backend_url for input: {input:?}");
            assert!(!d.backend_name.is_empty(), "empty backend_name for input: {input:?}");
            assert!(!d.model.is_empty(), "empty model for input: {input:?}");
        }
    }

    /// P5d-20: embedded control characters — no panics.
    #[test]
    fn p5d_control_characters() {
        let router = build_test_router();
        let d = router.classify("turn on the \x00light\x07please");
        // "turn on" + "light" still substring-match → HA.
        assert_eq!(d.intent, "home_automation");
    }

    /// P5d-21: very long single word (pathological substring probe).
    #[test]
    fn p5d_pathological_long_word() {
        let router = build_test_router();
        let huge_word = "a".repeat(100_000);
        let d = router.classify(&huge_word);
        // No keyword matches; default — but it MUST complete.
        assert_eq!(d.intent, "default");
    }

    /// P5d-22: `resolve_backend_for_model` with nonexistent model returns
    /// None — caller is responsible for translating into HTTP 400.
    #[test]
    fn p5d_explicit_model_unknown_returns_none() {
        let router = build_test_router();
        assert!(router.resolve_backend_for_model("no-such-model-ever").is_none());
    }

    /// P5d-23: `resolve_backend_for_model` for a configured model returns
    /// the right backend.
    #[test]
    fn p5d_explicit_model_known_resolves() {
        let router = build_test_router();
        let d = router.resolve_backend_for_model("coder-model").expect("model exists");
        assert_eq!(d.router_method, RouterMethod::Explicit);
        assert_eq!(d.intent, "explicit");
        assert_eq!(d.model, "coder-model");
        assert_eq!(d.backend_name, "coder-backend");
        assert!(d.explicit_flow.is_none());
    }

    /// P5d-24: Sprint 063 P1 explicit-flow helper produces the expected
    /// decision shape — ExplicitFlow method, explicit_flow = Some(name),
    /// default backend (flow drives actual model selection per-step).
    #[test]
    fn p5d_explicit_flow_decision_shape() {
        let router = build_test_router();
        let d = router.explicit_flow_decision("dream_exploration");
        assert_eq!(d.router_method, RouterMethod::ExplicitFlow);
        assert_eq!(d.intent, "explicit_flow");
        assert_eq!(d.explicit_flow.as_deref(), Some("dream_exploration"));
        assert_eq!(d.keyword_match_count, 0);
        assert_eq!(d.keyword_match_kind, KeywordMatchKind::None);
        // Model/backend default through — flow engine picks per-step.
        assert_eq!(d.model, "default-model");
        assert_eq!(d.backend_name, "default-backend");
    }
}
