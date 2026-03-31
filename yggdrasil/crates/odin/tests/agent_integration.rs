/// Integration tests for the agent loop.
///
/// Uses `ygg_test_harness` mock servers for Ollama and Mimir,
/// constructs a test AppState, and exercises `run_agent_loop` end-to-end.
use std::sync::Arc;

use serde_json::json;

use odin::agent::run_agent_loop;
use odin::openai::{ChatMessage, FunctionDefinition, Role, ToolDefinition};
use odin::router::RoutingDecision;
use odin::session::SessionStore;
use odin::state::AppState;
use odin::tool_registry::{build_registry, ToolTier};
use ygg_domain::config::{
    AgentLoopConfig, BackendType, MimirClientConfig, MuninnClientConfig, OdinConfig, RoutingConfig,
    SessionConfig,
};
use ygg_test_harness::{MockMimirBuilder, MockOllamaBuilder};

/// Build a minimal test AppState pointing at the mock servers.
fn test_state(_ollama_url: &str, mimir_url: &str) -> AppState {
    let config = OdinConfig {
        node_name: "test".to_string(),
        listen_addr: "127.0.0.1:0".to_string(),
        backends: vec![],
        routing: RoutingConfig {
            default_model: "test-model".to_string(),
            default_backend: None,
            rules: vec![],
        },
        mimir: MimirClientConfig {
            url: mimir_url.to_string(),
            query_limit: 5,
            store_on_completion: false,
        },
        muninn: MuninnClientConfig {
            url: "http://127.0.0.1:1".to_string(), // unused in these tests
            max_context_chunks: 10,
        },
        ha: None,
        session: SessionConfig::default(),
        cloud: None,
        voice: None,
        agent: Some(AgentLoopConfig::default()),
        task_worker: None,
        web_search: None,
        llm_router: None,
    };

    AppState {
        http_client: reqwest::Client::new(),
        router: odin::router::SemanticRouter::new(&config.routing, &config.backends),
        backends: vec![],
        mimir_url: mimir_url.to_string(),
        muninn_url: "http://127.0.0.1:1".to_string(),
        ha_client: None,
        ha_context_cache: Arc::new(tokio::sync::RwLock::new(None)),
        session_store: SessionStore::new(SessionConfig::default()),
        cloud_pool: None,
        voice_api_url: None,
        omni_url: None,
        config,
        tool_registry: Arc::new(build_registry()),
        gaming_config: None,
        skill_cache: Arc::new(odin::skill_cache::SkillCache::new()),
        wake_word_registry: Arc::new(odin::wake_word::WakeWordRegistry::new(None)),
        omni_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        voice_alert_tx: tokio::sync::broadcast::channel::<String>(16).0,
        web_search_config: None,
        circuit_breakers: odin::state::CircuitBreakerRegistry::new(),
        sdr_router: Arc::new(odin::sdr_router::SdrRouter::with_defaults()),
        llm_router: None,
        router_queue: None,
        request_log: None,
    }
}

fn test_decision(ollama_url: &str) -> RoutingDecision {
    RoutingDecision {
        intent: "default".to_string(),
        confidence: None,
        router_method: odin::router::RouterMethod::Keyword,
        model: "test-model".to_string(),
        backend_url: ollama_url.to_string(),
        backend_name: "mock-ollama".to_string(),
        backend_type: BackendType::Ollama,
    }
}

fn tool_defs_with_query_memory() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: "query_memory".to_string(),
            description: "Search engram memory".to_string(),
            parameters: json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
        },
    }]
}

fn default_config() -> AgentLoopConfig {
    AgentLoopConfig {
        max_iterations: 5,
        max_tool_calls_total: 10,
        tool_timeout_secs: 5,
        total_timeout_secs: 30,
        default_tiers: vec!["safe".to_string()],
        ..AgentLoopConfig::default()
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_text_response_no_tool_calls() {
    let ollama = MockOllamaBuilder::new()
        .with_text_response("Hello! I can help you.")
        .start()
        .await;
    let mimir = MockMimirBuilder::new().start().await;
    let (ollama_url, mimir_url) = (&ollama.url, &mimir.url);
    let state = test_state(&ollama_url, &mimir_url);

    let messages = vec![ChatMessage::new(Role::User, "Hi")];
    let tools = tool_defs_with_query_memory();
    let registry = build_registry();
    let tiers = [ToolTier::Safe];
    let decision = test_decision(&ollama_url);
    let config = default_config();

    let result = run_agent_loop(
        &state, &messages, &tools, &registry, &tiers, &decision, "test-1", &config, 16384,
    )
    .await;

    let resp = result.expect("agent loop should succeed");
    assert_eq!(resp.choices[0].message.content, "Hello! I can help you.");
}

#[tokio::test]
async fn agent_single_tool_call_then_text() {
    let ollama = MockOllamaBuilder::new()
        .with_tool_call("query_memory", json!({"text": "recent sprints"}))
        .with_text_response("Based on memory, Sprint 043 added agentic tool-use.")
        .start()
        .await;
    let mimir = MockMimirBuilder::new().start().await;
    let (ollama_url, mimir_url) = (&ollama.url, &mimir.url);
    let state = test_state(&ollama_url, &mimir_url);

    let messages = vec![ChatMessage::new(Role::User, "What was the last sprint?")];
    let tools = tool_defs_with_query_memory();
    let registry = build_registry();
    let tiers = [ToolTier::Safe];
    let decision = test_decision(&ollama_url);
    let config = default_config();

    let result = run_agent_loop(
        &state, &messages, &tools, &registry, &tiers, &decision, "test-2", &config, 16384,
    )
    .await;

    let resp = result.expect("agent loop should succeed");
    assert!(
        resp.choices[0].message.content.contains("Sprint 043"),
        "expected response to contain tool result synthesis"
    );
}

#[tokio::test]
async fn agent_max_iterations_forces_text() {
    // 3 tool_call responses + 1 forced text (default fallback in mock).
    let ollama = MockOllamaBuilder::new()
        .with_tool_call("query_memory", json!({"text": "loop"}))
        .with_tool_call("query_memory", json!({"text": "loop"}))
        .with_tool_call("query_memory", json!({"text": "loop"}))
        // After max_iterations, agent sends without tools — mock returns default fallback.
        .start()
        .await;
    let mimir = MockMimirBuilder::new().start().await;
    let (ollama_url, mimir_url) = (&ollama.url, &mimir.url);
    let state = test_state(&ollama_url, &mimir_url);

    let messages = vec![ChatMessage::new(Role::User, "loop forever")];
    let tools = tool_defs_with_query_memory();
    let registry = build_registry();
    let tiers = [ToolTier::Safe];
    let decision = test_decision(&ollama_url);
    let config = AgentLoopConfig {
        max_iterations: 3,
        max_tool_calls_total: 30,
        tool_timeout_secs: 5,
        total_timeout_secs: 30,
        default_tiers: vec!["safe".to_string()],
        ..AgentLoopConfig::default()
    };

    let result = run_agent_loop(
        &state, &messages, &tools, &registry, &tiers, &decision, "test-3", &config, 16384,
    )
    .await;

    let resp = result.expect("agent loop should succeed even at max iterations");
    // The mock's default fallback returns "default fallback".
    assert_eq!(resp.choices[0].message.content, "default fallback");
}

#[tokio::test]
async fn agent_unknown_tool_returns_error_to_model() {
    let ollama = MockOllamaBuilder::new()
        .with_tool_call("nonexistent_tool", json!({}))
        .with_text_response("Sorry, that tool is not available.")
        .start()
        .await;
    let mimir = MockMimirBuilder::new().start().await;
    let (ollama_url, mimir_url) = (&ollama.url, &mimir.url);
    let state = test_state(&ollama_url, &mimir_url);

    let messages = vec![ChatMessage::new(Role::User, "use nonexistent tool")];
    let tools = tool_defs_with_query_memory();
    let registry = build_registry();
    let tiers = [ToolTier::Safe];
    let decision = test_decision(&ollama_url);
    let config = default_config();

    let result = run_agent_loop(
        &state, &messages, &tools, &registry, &tiers, &decision, "test-4", &config, 16384,
    )
    .await;

    let resp = result.expect("agent loop should handle unknown tools gracefully");
    assert!(resp.choices[0].message.content.contains("not available"));
}
