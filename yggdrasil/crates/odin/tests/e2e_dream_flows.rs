//! E2E integration tests for the four Dream flows (mocked).
//!
//! Sprint 063 Track C — P5c.
//!
//! Uses `ChatCompletionRequest.flow = Some("dream_...")` (Track A P1 field)
//! to explicitly dispatch each dream flow bypassing intent routing.
//!
//! Flows tested:
//!   - dream_consolidation  (3 steps: query_recent, find_patterns, store_insights)
//!   - dream_exploration    (3 steps: brainstorm, evaluate, store)
//!   - dream_speculation    (3 steps: deep_reason, summarize, store)
//!   - dream_self_improvement (4 steps: gather, critique, rank, store)
//!
//! Each test:
//!   1. Queues one canned MockOllama response per flow step.
//!   2. Builds AppState with the relevant FlowConfig.
//!   3. POSTs to chat_handler with `flow = Some("dream_...")`.
//!   4. Asserts HTTP 200, non-empty assistant content, and all mock
//!      responses consumed (confirming the correct step count).
//!
//! Note: These tests compile once `ChatCompletionRequest.flow: Option<String>`
//! exists (Track A P1). The field was confirmed present in openai.rs before
//! this file was written.

use std::sync::Arc;

use axum::extract::{Json, State};
use axum::response::IntoResponse;
use serde_json::Value as JsonValue;

use odin::openai::{ChatMessage, Role};
use odin::session::SessionStore;
use odin::state::AppState;
use odin::tool_registry::build_registry;
use ygg_domain::config::{
    AgentLoopConfig, BackendConfig, BackendType, FlowConfig, FlowInput, FlowStep, FlowTrigger,
    MimirClientConfig, MuninnClientConfig, OdinConfig, RoutingConfig, SessionConfig,
};
use ygg_test_harness::{MockMimirBuilder, MockMuninnBuilder, MockOllamaBuilder, assert_flow_executed_json};

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a minimal FlowStep with sensible defaults.  All optional fields are
/// left as None / empty.
fn simple_step(name: &str, input: FlowInput) -> FlowStep {
    FlowStep {
        name: name.to_string(),
        backend: "mock-ollama".to_string(),
        model: "nemotron-ultra".to_string(),
        system_prompt: None,
        input,
        output_key: name.to_string(),
        max_tokens: 512,
        temperature: 0.3,
        tools: None,
        think: None,
        agent_config: None,
        stream_role: None,
        stream_label: None,
        parallel_with: None,
        watches: None,
        sentinel: None,
        sentinel_skips: None,
    }
}

/// Build a step that uses GLM as its model (for brainstorm / deep_reason steps).
fn glm_step(name: &str, input: FlowInput) -> FlowStep {
    FlowStep {
        model: "glm4:27b".to_string(),
        ..simple_step(name, input)
    }
}

/// Build a minimal OdinConfig with a single mock backend and the given flows.
fn dream_config(ollama_url: &str, mimir_url: &str, muninn_url: &str, flows: Vec<FlowConfig>) -> OdinConfig {
    OdinConfig {
        node_name: "test-dream".to_string(),
        listen_addr: "127.0.0.1:0".to_string(),
        backends: vec![BackendConfig {
            name: "mock-ollama".to_string(),
            url: ollama_url.to_string(),
            backend_type: BackendType::Ollama,
            models: vec!["nemotron-ultra".to_string(), "glm4:27b".to_string()],
            max_concurrent: 4,
            context_window: 16384,
        }],
        routing: RoutingConfig {
            default_model: "nemotron-ultra".to_string(),
            default_backend: Some("mock-ollama".to_string()),
            intent_default: None,
            rules: vec![],
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
        flows,
        cameras: None,
    }
}

fn dream_test_state(ollama_url: &str, mimir_url: &str, muninn_url: &str, flows: Vec<FlowConfig>) -> AppState {
    let config = dream_config(ollama_url, mimir_url, muninn_url, flows);
    let backend = odin::state::BackendState {
        name: "mock-ollama".to_string(),
        url: ollama_url.to_string(),
        backend_type: BackendType::Ollama,
        models: vec!["nemotron-ultra".to_string(), "glm4:27b".to_string()],
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
        config_path: std::path::PathBuf::from("/tmp/test-dream-odin-config.json"),
    }
}

/// Build a non-streaming ChatCompletionRequest with an explicit flow override.
fn dream_request(user_msg: &str, flow_name: &str) -> odin::openai::ChatCompletionRequest {
    odin::openai::ChatCompletionRequest {
        model: None,
        messages: vec![ChatMessage::new(Role::User, user_msg)],
        stream: false,
        temperature: None,
        max_tokens: None,
        top_p: None,
        stop: None,
        session_id: None,
        project_id: None,
        tools: None,
        tool_choice: None,
        // Explicit flow override — bypasses intent routing entirely.
        flow: Some(flow_name.to_string()),
    }
}

/// Call chat_handler and return the parsed JSON response body.
async fn run_chat(state: AppState, request: odin::openai::ChatCompletionRequest) -> (axum::http::StatusCode, JsonValue) {
    let response = odin::handlers::chat_handler(State(state), Json(request))
        .await
        .expect("chat_handler should not error")
        .into_response();

    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body bytes");
    let body: JsonValue = serde_json::from_slice(&bytes).expect("parse JSON body");
    (status, body)
}

// ─────────────────────────────────────────────────────────────────────────────
// P5c Test cases
// ─────────────────────────────────────────────────────────────────────────────

/// dream_consolidation — 3 steps, all nemotron.
///
/// Steps: query_recent → find_patterns → store_insights.
/// Trigger: Manual (can also be Idle; tests use explicit flow override).
#[tokio::test]
async fn test_e2e_dream_consolidation() {
    let flow = FlowConfig {
        name: "dream_consolidation".to_string(),
        trigger: FlowTrigger::Manual,
        steps: vec![
            simple_step("query_recent", FlowInput::UserMessage),
            simple_step("find_patterns", FlowInput::StepOutput { key: "query_recent".to_string() }),
            simple_step("store_insights", FlowInput::StepOutput { key: "find_patterns".to_string() }),
        ],
        timeout_secs: 60,
        max_step_output_chars: 4000,
        loop_config: None,
    };

    let ollama = MockOllamaBuilder::new()
        .expect_flow_steps(vec![
            ("query_recent",   "Recent memory: 42 engrams from the last 24 hours."),
            ("find_patterns",  "Pattern: Sprint velocity increasing. Memory gaps in HA automation."),
            ("store_insights", "Stored: [dream][consolidation] Sprint velocity trend + HA gap noted."),
        ])
        .start()
        .await;
    let mimir  = MockMimirBuilder::new().start().await;
    let muninn = MockMuninnBuilder::new().start().await;
    let state  = dream_test_state(&ollama.url, &mimir.url, &muninn.url, vec![flow]);

    let req = dream_request("run dream consolidation", "dream_consolidation");
    let (status, body) = run_chat(state, req).await;

    assert_eq!(status, axum::http::StatusCode::OK, "dream_consolidation should return 200");

    assert_flow_executed_json(&body, "dream_consolidation", &["query_recent", "find_patterns", "store_insights"])
        .expect("dream_consolidation flow assertion failed");

    let remaining = ollama.responses.lock().unwrap().len();
    assert_eq!(remaining, 0, "all 3 dream_consolidation steps should have consumed mock responses");
}

/// dream_exploration — 3 steps: GLM brainstorm → nemotron evaluate → nemotron store.
///
/// Trigger: Manual (explicit flow override).
#[tokio::test]
async fn test_e2e_dream_exploration() {
    let flow = FlowConfig {
        name: "dream_exploration".to_string(),
        trigger: FlowTrigger::Manual,
        steps: vec![
            glm_step("brainstorm", FlowInput::UserMessage),
            simple_step("evaluate", FlowInput::StepOutput { key: "brainstorm".to_string() }),
            simple_step("store",    FlowInput::StepOutput { key: "evaluate".to_string() }),
        ],
        timeout_secs: 60,
        max_step_output_chars: 4000,
        loop_config: None,
    };

    let ollama = MockOllamaBuilder::new()
        .expect_flow_steps(vec![
            ("brainstorm", "Idea 1: Predictive prefetch from session SDR drift. Idea 2: HA intent via voice tone. Idea 3: Proactive engram expiry."),
            ("evaluate",   "Top idea: SDR drift prefetch — high signal/noise, low infra cost. Reject tone idea (privacy)."),
            ("store",      "Stored: [dream][exploration] SDR prefetch idea evaluated → pursue in Sprint 064."),
        ])
        .start()
        .await;
    let mimir  = MockMimirBuilder::new().start().await;
    let muninn = MockMuninnBuilder::new().start().await;
    let state  = dream_test_state(&ollama.url, &mimir.url, &muninn.url, vec![flow]);

    let req = dream_request(
        "How could Yggdrasil's memory system predict user needs before they ask?",
        "dream_exploration",
    );
    let (status, body) = run_chat(state, req).await;

    assert_eq!(status, axum::http::StatusCode::OK, "dream_exploration should return 200");

    assert_flow_executed_json(&body, "dream_exploration", &["brainstorm", "evaluate", "store"])
        .expect("dream_exploration flow assertion failed");

    let remaining = ollama.responses.lock().unwrap().len();
    assert_eq!(remaining, 0, "all 3 dream_exploration steps should have consumed mock responses");
}

/// dream_speculation — 3 steps: GLM deep_reason → nemotron summarize → nemotron store.
///
/// Trigger: Manual (explicit flow override).
#[tokio::test]
async fn test_e2e_dream_speculation() {
    let flow = FlowConfig {
        name: "dream_speculation".to_string(),
        trigger: FlowTrigger::Manual,
        steps: vec![
            glm_step("deep_reason", FlowInput::UserMessage),
            simple_step("summarize", FlowInput::StepOutput { key: "deep_reason".to_string() }),
            simple_step("store",     FlowInput::StepOutput { key: "summarize".to_string() }),
        ],
        timeout_secs: 90,
        max_step_output_chars: 4000,
        loop_config: None,
    };

    let ollama = MockOllamaBuilder::new()
        .expect_flow_steps(vec![
            ("deep_reason", "Grokking on hybrid conv/attention: The hypothesis is that phase transitions require a specific gradient norm profile that conflicts with LoRA's rank constraints. Full fine-tuning removes this constraint but requires at least 10x more compute than we attempted in Sprint 054. The LFM2.5 architecture shows promise for grokking because its hybrid conv layers provide implicit regularization."),
            ("summarize",   "Summary: Grokking on hybrid models is feasible but requires full fine-tuning, not LoRA, and ~10x more compute. LFM2.5 is the best candidate. Recommend Morrigan 72h run at 1e-4 LR."),
            ("store",       "Stored: [dream][speculation] Grokking hypothesis — full fine-tune 10x compute threshold."),
        ])
        .start()
        .await;
    let mimir  = MockMimirBuilder::new().start().await;
    let muninn = MockMuninnBuilder::new().start().await;
    let state  = dream_test_state(&ollama.url, &mimir.url, &muninn.url, vec![flow]);

    let req = dream_request(
        "Can grokking be achieved on hybrid conv/attention architectures with full fine-tuning?",
        "dream_speculation",
    );
    let (status, body) = run_chat(state, req).await;

    assert_eq!(status, axum::http::StatusCode::OK, "dream_speculation should return 200");

    assert_flow_executed_json(&body, "dream_speculation", &["deep_reason", "summarize", "store"])
        .expect("dream_speculation flow assertion failed");

    let remaining = ollama.responses.lock().unwrap().len();
    assert_eq!(remaining, 0, "all 3 dream_speculation steps should have consumed mock responses");
}

/// dream_self_improvement — 4 steps: nemotron gather → GLM critique → nemotron rank → nemotron store.
///
/// Trigger: Manual (explicit flow override).
#[tokio::test]
async fn test_e2e_dream_self_improvement() {
    let flow = FlowConfig {
        name: "dream_self_improvement".to_string(),
        trigger: FlowTrigger::Manual,
        steps: vec![
            simple_step("gather",  FlowInput::UserMessage),
            glm_step("critique",   FlowInput::StepOutput { key: "gather".to_string() }),
            simple_step("rank",    FlowInput::StepOutput { key: "critique".to_string() }),
            simple_step("store",   FlowInput::StepOutput { key: "rank".to_string() }),
        ],
        timeout_secs: 90,
        max_step_output_chars: 4000,
        loop_config: None,
    };

    let ollama = MockOllamaBuilder::new()
        .expect_flow_steps(vec![
            ("gather",   "Gathered: Sprint 061 retro — flow streaming latency 1.2s p99. Agent loop max_iterations hit on 3 out of 200 sessions. SDR router mis-classifies gaming+HA mixed intent 8% of the time."),
            ("critique", "Critique: Latency acceptable. Agent loop limit is working as intended (safety). SDR mis-classification is the only real bug — keyword score needs a tiebreaker for overlapping domains."),
            ("rank",     "Priority: 1) SDR tiebreaker for overlapping intents. 2) Agent loop abort signal. 3) Flow latency profiling."),
            ("store",    "Stored: [dream][self_improvement] Sprint 061 retro analysis — SDR tiebreaker is top priority."),
        ])
        .start()
        .await;
    let mimir  = MockMimirBuilder::new().start().await;
    let muninn = MockMuninnBuilder::new().start().await;
    let state  = dream_test_state(&ollama.url, &mimir.url, &muninn.url, vec![flow]);

    let req = dream_request("analyze recent sprint retro and suggest improvements", "dream_self_improvement");
    let (status, body) = run_chat(state, req).await;

    assert_eq!(status, axum::http::StatusCode::OK, "dream_self_improvement should return 200");

    assert_flow_executed_json(
        &body,
        "dream_self_improvement",
        &["gather", "critique", "rank", "store"],
    )
    .expect("dream_self_improvement flow assertion failed");

    let remaining = ollama.responses.lock().unwrap().len();
    assert_eq!(
        remaining, 0,
        "all 4 dream_self_improvement steps should have consumed mock responses"
    );
}

/// Negative test: requesting a flow name that does not exist in the config
/// should not crash — Odin falls back to standard routing.
#[tokio::test]
async fn test_e2e_dream_unknown_flow_falls_back_gracefully() {
    let ollama = MockOllamaBuilder::new()
        .with_text_response("I processed your request without a flow.")
        .start()
        .await;
    let mimir  = MockMimirBuilder::new().start().await;
    let muninn = MockMuninnBuilder::new().start().await;

    // No flows registered — unknown flow names should fall back.
    let state = dream_test_state(&ollama.url, &mimir.url, &muninn.url, vec![]);

    let req = dream_request("run dream consolidation", "dream_nonexistent_flow_xyz");
    let (status, _body) = run_chat(state, req).await;

    // We don't assert the exact body — just that Odin doesn't 500.
    assert_ne!(
        status,
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "unknown flow name should not cause a 500 — Odin should fall back gracefully"
    );
}
