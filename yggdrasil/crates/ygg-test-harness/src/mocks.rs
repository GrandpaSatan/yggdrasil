//! Mock HTTP server builders for Yggdrasil services.
//!
//! Each builder spawns an axum server on a random port and returns a handle
//! with the URL and (optionally) a shared response queue for assertions.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::{Json, State};
use axum::routing::{get, post};
use axum::Router;
use serde_json::Value as JsonValue;
use tokio::net::TcpListener;

// ── MockOllama ──────────────────────────────────────────────────────

/// A running mock Ollama server.
pub struct MockOllama {
    pub url: String,
    pub responses: Arc<Mutex<VecDeque<JsonValue>>>,
}

/// Builder for a mock Ollama server with queued responses.
pub struct MockOllamaBuilder {
    responses: VecDeque<JsonValue>,
}

impl MockOllamaBuilder {
    pub fn new() -> Self {
        Self {
            responses: VecDeque::new(),
        }
    }

    /// Queue a plain text response (no tool calls).
    pub fn with_text_response(mut self, content: &str) -> Self {
        self.responses.push_back(serde_json::json!({
            "model": "test-model",
            "message": { "role": "assistant", "content": content },
            "done": true
        }));
        self
    }

    /// Queue a tool-call response.
    pub fn with_tool_call(mut self, name: &str, args: JsonValue) -> Self {
        self.responses.push_back(serde_json::json!({
            "model": "test-model",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": { "name": name, "arguments": args }
                }]
            },
            "done": true
        }));
        self
    }

    /// Queue a raw JSON response.
    pub fn with_raw_response(mut self, value: JsonValue) -> Self {
        self.responses.push_back(value);
        self
    }

    /// Queue multiple canned step responses for a multi-step flow test.
    ///
    /// Each tuple is `(step_name, response_content)`.  The `step_name` is
    /// embedded in the response `model` field so assertions can verify which
    /// step executed which model.  The `response_content` is the assistant
    /// message text returned for that step.
    ///
    /// Use alongside `flow_assertions::assert_flow_executed` to validate
    /// that the correct number of steps ran with the expected output.
    pub fn expect_flow_steps(mut self, steps: Vec<(&str, &str)>) -> Self {
        for (step_name, content) in steps {
            self.responses.push_back(serde_json::json!({
                "model": step_name,
                "message": { "role": "assistant", "content": content },
                "done": true
            }));
        }
        self
    }

    /// Spawn the mock server. Returns a handle with URL and response queue.
    pub async fn start(self) -> MockOllama {
        let queue = Arc::new(Mutex::new(self.responses));
        let state = queue.clone();

        let app = Router::new()
            .route("/api/chat", post(ollama_handler))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock ollama");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(axum::serve(listener, app).into_future());

        MockOllama {
            url: format!("http://127.0.0.1:{}", addr.port()),
            responses: queue,
        }
    }
}

impl Default for MockOllamaBuilder {
    fn default() -> Self {
        Self::new()
    }
}

async fn ollama_handler(
    State(queue): State<Arc<Mutex<VecDeque<JsonValue>>>>,
    Json(_body): Json<JsonValue>,
) -> Json<JsonValue> {
    let resp = queue
        .lock()
        .expect("lock")
        .pop_front()
        .unwrap_or_else(|| {
            serde_json::json!({
                "model": "test-model",
                "message": { "role": "assistant", "content": "default fallback" },
                "done": true
            })
        });
    Json(resp)
}

// ── MockMimir ───────────────────────────────────────────────────────

/// A running mock Mimir server.
pub struct MockMimir {
    pub url: String,
    pub responses: Arc<Mutex<VecDeque<JsonValue>>>,
}

/// Builder for a mock Mimir server.
///
/// Routes: `/api/v1/query`, `/api/v1/store`, `/api/v1/context`,
/// `/api/v1/sdr/operations`, `/api/v1/timeline`, `/api/v1/vault`,
/// `/api/v1/tasks`, `/api/v1/graph`, `/api/v1/sprints/list`.
pub struct MockMimirBuilder {
    responses: VecDeque<JsonValue>,
    default_response: Option<JsonValue>,
}

impl MockMimirBuilder {
    pub fn new() -> Self {
        Self {
            responses: VecDeque::new(),
            default_response: None,
        }
    }

    /// Queue a response (served FIFO for any route).
    pub fn with_response(mut self, value: JsonValue) -> Self {
        self.responses.push_back(value);
        self
    }

    /// Set the fallback response when the queue is empty.
    pub fn with_default_response(mut self, value: JsonValue) -> Self {
        self.default_response = Some(value);
        self
    }

    /// Pre-configure vault CRUD responses for vault-aware flow tests.
    ///
    /// Queues a standard vault list response followed by an upsert acknowledgement.
    /// This covers the common pattern: flow step reads vault entries, then stores
    /// a new insight back.  Extend with `.with_response()` for additional calls.
    pub fn mock_mimir_vault(mut self) -> Self {
        // Vault list response: one existing entry.
        self.responses.push_back(serde_json::json!({
            "entries": [
                { "key": "test_vault_key", "value": "existing vault value", "updated_at": 0 }
            ]
        }));
        // Vault upsert acknowledgement.
        self.responses.push_back(serde_json::json!({ "ok": true }));
        self
    }

    pub async fn start(self) -> MockMimir {
        let default = self.default_response.unwrap_or_else(|| {
            serde_json::json!({
                "results": [{"cause": "test query", "effect": "test result", "similarity": 0.92}]
            })
        });
        let queue = Arc::new(Mutex::new(self.responses));
        let state = MimirState {
            responses: queue.clone(),
            default: default.clone(),
        };

        let app = Router::new()
            .route("/api/v1/query", post(mimir_catchall_handler))
            .route("/api/v1/store", post(mimir_catchall_handler))
            .route("/api/v1/context", post(mimir_catchall_handler))
            .route("/api/v1/sdr/operations", post(mimir_catchall_handler))
            .route("/api/v1/timeline", post(mimir_catchall_handler))
            .route("/api/v1/vault", post(mimir_catchall_handler))
            .route("/api/v1/tasks", post(mimir_catchall_handler))
            .route("/api/v1/graph", post(mimir_catchall_handler))
            .route("/api/v1/sprints/list", post(mimir_catchall_handler))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock mimir");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(axum::serve(listener, app).into_future());

        MockMimir {
            url: format!("http://127.0.0.1:{}", addr.port()),
            responses: queue,
        }
    }
}

impl Default for MockMimirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
struct MimirState {
    responses: Arc<Mutex<VecDeque<JsonValue>>>,
    default: JsonValue,
}

async fn mimir_catchall_handler(
    State(state): State<MimirState>,
    Json(_body): Json<JsonValue>,
) -> Json<JsonValue> {
    let resp = state
        .responses
        .lock()
        .expect("lock")
        .pop_front()
        .unwrap_or_else(|| state.default.clone());
    Json(resp)
}

// ── MockMuninn ──────────────────────────────────────────────────────

/// A running mock Muninn server.
pub struct MockMuninn {
    pub url: String,
    pub responses: Arc<Mutex<VecDeque<JsonValue>>>,
}

/// Builder for a mock Muninn server.
///
/// Routes: `/api/v1/search`, `/api/v1/symbols`, `/api/v1/references`.
pub struct MockMuninnBuilder {
    responses: VecDeque<JsonValue>,
    default_response: Option<JsonValue>,
}

impl MockMuninnBuilder {
    pub fn new() -> Self {
        Self {
            responses: VecDeque::new(),
            default_response: None,
        }
    }

    pub fn with_response(mut self, value: JsonValue) -> Self {
        self.responses.push_back(value);
        self
    }

    pub fn with_default_response(mut self, value: JsonValue) -> Self {
        self.default_response = Some(value);
        self
    }

    pub async fn start(self) -> MockMuninn {
        let default = self.default_response.unwrap_or_else(|| {
            serde_json::json!({
                "chunks": [],
                "total": 0
            })
        });
        let queue = Arc::new(Mutex::new(self.responses));
        let state = MuninnState {
            responses: queue.clone(),
            default: default.clone(),
        };

        let app = Router::new()
            .route("/api/v1/search", post(muninn_catchall_handler))
            .route("/api/v1/symbols", get(muninn_catchall_get_handler))
            .route("/api/v1/references", get(muninn_catchall_get_handler))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock muninn");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(axum::serve(listener, app).into_future());

        MockMuninn {
            url: format!("http://127.0.0.1:{}", addr.port()),
            responses: queue,
        }
    }
}

impl Default for MockMuninnBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
struct MuninnState {
    responses: Arc<Mutex<VecDeque<JsonValue>>>,
    default: JsonValue,
}

async fn muninn_catchall_handler(
    State(state): State<MuninnState>,
    Json(_body): Json<JsonValue>,
) -> Json<JsonValue> {
    let resp = state
        .responses
        .lock()
        .expect("lock")
        .pop_front()
        .unwrap_or_else(|| state.default.clone());
    Json(resp)
}

async fn muninn_catchall_get_handler(State(state): State<MuninnState>) -> Json<JsonValue> {
    let resp = state
        .responses
        .lock()
        .expect("lock")
        .pop_front()
        .unwrap_or_else(|| state.default.clone());
    Json(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_ollama_serves_queued_responses() {
        let mock = MockOllamaBuilder::new()
            .with_text_response("first")
            .with_text_response("second")
            .start()
            .await;

        let client = reqwest::Client::new();

        let r1: JsonValue = client
            .post(format!("{}/api/chat", mock.url))
            .json(&serde_json::json!({"model": "test", "messages": []}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r1["message"]["content"], "first");

        let r2: JsonValue = client
            .post(format!("{}/api/chat", mock.url))
            .json(&serde_json::json!({"model": "test", "messages": []}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r2["message"]["content"], "second");

        // Third call hits fallback
        let r3: JsonValue = client
            .post(format!("{}/api/chat", mock.url))
            .json(&serde_json::json!({"model": "test", "messages": []}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r3["message"]["content"], "default fallback");
    }

    #[tokio::test]
    async fn mock_mimir_serves_query_results() {
        let mock = MockMimirBuilder::new()
            .with_response(serde_json::json!({"results": [{"cause": "custom", "effect": "result", "similarity": 0.95}]}))
            .start()
            .await;

        let client = reqwest::Client::new();
        let resp: JsonValue = client
            .post(format!("{}/api/v1/query", mock.url))
            .json(&serde_json::json!({"text": "test query"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        assert_eq!(resp["results"][0]["cause"], "custom");
        assert_eq!(resp["results"][0]["similarity"], 0.95);
    }
}
