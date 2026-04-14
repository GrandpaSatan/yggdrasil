//! E2E integration test for the home_automation flow (mocked).
//!
//! Sprint 063 Track C — P5b.
//!
//! Wires up MockOllama with 3 canned step responses
//! (extract_action → execute → confirm) alongside MockMimir and MockMuninn
//! stubs, POSTs "turn on the kitchen light" to the Odin chat handler, and
//! asserts that:
//!   - HTTP 200 is returned.
//!   - Response content matches a home-automation confirm pattern.
//!   - MockOllama queue is fully consumed (3 responses → 3 steps).
//!   - Routing decision intent is home_automation.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::response::IntoResponse;
use serde_json::{json, Value as JsonValue};

use odin::openai::{ChatMessage, Role};
use odin::session::SessionStore;
use odin::state::AppState;
use odin::tool_registry::build_registry;
use ygg_domain::config::{
    AgentLoopConfig, BackendConfig, BackendType, FlowConfig, FlowInput, FlowStep, FlowTrigger,
    MimirClientConfig, MuninnClientConfig, OdinConfig, RoutingConfig, RoutingRule, SessionConfig,
};
use ygg_test_harness::{
    MockMimirBuilder, MockMuninnBuilder, MockOllamaBuilder,
    assert_flow_executed_json, assert_content_contains_any_json,
};

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a minimal OdinConfig that routes home_automation intent to the mock
/// Ollama backend and has the home_automation flow configured.
fn ha_test_config(ollama_url: &str, mimir_url: &str, muninn_url: &str) -> OdinConfig {
    OdinConfig {
        node_name: "test-ha".to_string(),
        listen_addr: "127.0.0.1:0".to_string(),
        backends: vec![BackendConfig {
            name: "mock-ollama".to_string(),
            url: ollama_url.to_string(),
            backend_type: BackendType::Ollama,
            models: vec!["gemma4:e4b".to_string()],
            max_concurrent: 4,
            context_window: 16384,
        }],
        routing: RoutingConfig {
            default_model: "gemma4:e4b".to_string(),
            default_backend: Some("mock-ollama".to_string()),
            intent_default: None,
            rules: vec![RoutingRule {
                intent: "home_automation".to_string(),
                model: "gemma4:e4b".to_string(),
                backend: "mock-ollama".to_string(),
            }],
        },
        mimir: MimirClientConfig {
            url: mimir_url.to_string(),
            query_limit: 5,
            store_on_completion: false,
        },
        muninn: MuninnClientConfig {
            url: muninn_url.to_string(),
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
        flows: vec![FlowConfig {
            name: "home_automation".to_string(),
            trigger: FlowTrigger::Intent("home_automation".to_string()),
            steps: vec![
                FlowStep {
                    name: "extract_action".to_string(),
                    backend: "mock-ollama".to_string(),
                    model: "gemma4:e4b".to_string(),
                    system_prompt: Some("Extract the HA entity and action from the user request.".to_string()),
                    input: FlowInput::UserMessage,
                    output_key: "action".to_string(),
                    max_tokens: 256,
                    temperature: 0.1,
                    tools: None,
                    think: None,
                    agent_config: None,
                    stream_role: None,
                    stream_label: None,
                    parallel_with: None,
                    watches: None,
                    sentinel: None,
                    sentinel_skips: None,
                },
                FlowStep {
                    name: "execute".to_string(),
                    backend: "mock-ollama".to_string(),
                    model: "gemma4:e4b".to_string(),
                    system_prompt: Some("Execute the extracted HA action.".to_string()),
                    input: FlowInput::StepOutput { key: "action".to_string() },
                    output_key: "result".to_string(),
                    max_tokens: 256,
                    temperature: 0.1,
                    tools: None,
                    think: None,
                    agent_config: None,
                    stream_role: None,
                    stream_label: None,
                    parallel_with: None,
                    watches: None,
                    sentinel: None,
                    sentinel_skips: None,
                },
                FlowStep {
                    name: "confirm".to_string(),
                    backend: "mock-ollama".to_string(),
                    model: "gemma4:e4b".to_string(),
                    system_prompt: Some("Confirm the action result to the user.".to_string()),
                    input: FlowInput::StepOutput { key: "result".to_string() },
                    output_key: "confirmation".to_string(),
                    max_tokens: 256,
                    temperature: 0.1,
                    tools: None,
                    think: None,
                    agent_config: None,
                    stream_role: None,
                    stream_label: None,
                    parallel_with: None,
                    watches: None,
                    sentinel: None,
                    sentinel_skips: None,
                },
            ],
            timeout_secs: 30,
            max_step_output_chars: 4000,
            loop_config: None,
        }],
        cameras: None,
    }
}

fn ha_test_state(ollama_url: &str, mimir_url: &str, muninn_url: &str) -> AppState {
    let config = ha_test_config(ollama_url, mimir_url, muninn_url);
    let backend = odin::state::BackendState {
        name: "mock-ollama".to_string(),
        url: ollama_url.to_string(),
        backend_type: BackendType::Ollama,
        models: vec!["gemma4:e4b".to_string()],
        semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        context_window: 16384,
    };

    let flow_engine = Arc::new(odin::flow::FlowEngine::new(
        reqwest::Client::new(),
        Arc::new(vec![backend.clone()]),
    ));

    let flows_snapshot = Arc::new(config.flows.clone());
    let flows = Arc::new(std::sync::RwLock::new(flows_snapshot));

    AppState {
        http_client: reqwest::Client::new(),
        router: odin::router::SemanticRouter::new(&config.routing, &config.backends),
        backends: vec![backend],
        mimir_url: mimir_url.to_string(),
        muninn_url: muninn_url.to_string(),
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
        flow_engine,
        activity_tracker: odin::flow_scheduler::ActivityTracker::new(),
        camera_cooldown: Arc::new(odin::camera::CooldownTracker::new()),
        flows,
        config_path: std::path::PathBuf::from("/tmp/test-ha-odin-config.json"),
    }
}

fn ha_chat_request(msg: &str) -> odin::openai::ChatCompletionRequest {
    odin::openai::ChatCompletionRequest {
        model: None,
        messages: vec![ChatMessage::new(Role::User, msg)],
        stream: false,
        temperature: None,
        max_tokens: None,
        top_p: None,
        stop: None,
        session_id: None,
        project_id: None,
        tools: None,
        tool_choice: None,
        // flow: None means intent-based dispatch (no explicit flow override).
        // Track A (P1) added this field. P5b tests use intent-based routing.
        flow: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

/// P5b mocked HA flow: 3-step pipeline (extract_action → execute → confirm).
///
/// Asserts:
///  - Response HTTP 200.
///  - Content matches a home-automation confirm pattern.
///  - MockOllama queue fully drained (all 3 steps consumed).
///  - Routing intent is home_automation.
#[tokio::test]
async fn test_e2e_ha_flow_three_steps_mocked() {
    let ollama = MockOllamaBuilder::new()
        .expect_flow_steps(vec![
            ("extract_action", "Action: light.turn_on entity_id=light.kitchen"),
            ("execute",        "Executed: light.turn_on on light.kitchen — service call sent"),
            ("confirm",        "The kitchen light is now on. Is there anything else, sir?"),
        ])
        .start()
        .await;

    let mimir  = MockMimirBuilder::new().start().await;
    let muninn = MockMuninnBuilder::new().start().await;
    let state  = ha_test_state(&ollama.url, &mimir.url, &muninn.url);
    let req    = ha_chat_request("turn on the kitchen light");

    let response = odin::handlers::chat_handler(State(state), Json(req))
        .await
        .expect("chat_handler should succeed")
        .into_response();

    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "expected HTTP 200 from chat_handler"
    );

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body bytes");
    let body: JsonValue = serde_json::from_slice(&body_bytes).expect("parse response JSON");

    assert_flow_executed_json(&body, "home_automation", &["extract_action", "execute", "confirm"])
        .expect("flow assertion should pass");

    assert_content_contains_any_json(
        &body,
        &["kitchen light", "light is now on", "turned on", "kitchen", "light.turn_on"],
    )
    .expect("response should mention the kitchen light action");

    let remaining = ollama.responses.lock().unwrap().len();
    assert_eq!(
        remaining, 0,
        "all 3 flow step responses should have been consumed by the flow engine"
    );
}

/// P5b router regression: "turn on kitchen light while I play Fallout" must
/// still route to home_automation, not gaming or default.
#[tokio::test]
async fn test_e2e_ha_flow_mixed_intent_still_routes_ha() {
    let ollama = MockOllamaBuilder::new()
        .expect_flow_steps(vec![
            ("extract_action", "Action extracted."),
            ("execute",        "Executed."),
            ("confirm",        "Done. Light is on."),
        ])
        .start()
        .await;

    let mimir  = MockMimirBuilder::new().start().await;
    let muninn = MockMuninnBuilder::new().start().await;
    let state  = ha_test_state(&ollama.url, &mimir.url, &muninn.url);

    let message = "turn on the kitchen light while I play Fallout";
    let decision = state.router.classify(message);

    assert_eq!(
        decision.intent, "home_automation",
        "mixed gaming+HA message should still route to home_automation, got: {}",
        decision.intent
    );

    let req = ha_chat_request(message);
    let response = odin::handlers::chat_handler(State(state), Json(req))
        .await
        .expect("chat_handler should succeed for mixed-intent message")
        .into_response();

    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "expected HTTP 200 for mixed-intent HA message"
    );
}

/// P5b: Mimir receives a RAG query during chat_handler execution.
///
/// Verifies that the queued Mimir response is consumed, confirming that the
/// memory-fetch path (step 5 of chat_handler) executed correctly.
#[tokio::test]
async fn test_e2e_ha_flow_mimir_receives_rag_query() {
    let ollama = MockOllamaBuilder::new()
        .expect_flow_steps(vec![
            ("extract_action", "Action extracted."),
            ("execute",        "Executed."),
            ("confirm",        "Light is on."),
        ])
        .start()
        .await;

    let mimir = MockMimirBuilder::new()
        .with_response(json!({
            "results": [
                { "cause": "kitchen light", "effect": "turn_on called", "similarity": 0.91 }
            ]
        }))
        .start()
        .await;
    let muninn = MockMuninnBuilder::new().start().await;
    let state  = ha_test_state(&ollama.url, &mimir.url, &muninn.url);
    let req    = ha_chat_request("turn on the kitchen light");

    let _ = odin::handlers::chat_handler(State(state), Json(req)).await;

    let remaining = mimir.responses.lock().unwrap().len();
    assert_eq!(
        remaining, 0,
        "Mimir queued response should have been consumed by the RAG fetch in chat_handler"
    );
}
