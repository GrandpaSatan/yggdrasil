//! SSE streaming primitives for the flow engine (Sprint 061).
//!
//! Protocol contract:
//!   - Intermediate "thinking" steps emit `event: ygg_step` frames carrying a
//!     typed `StreamEvent` payload. OpenAI-compliant clients ignore non-default
//!     event names and see only the terminal step's tokens.
//!   - Terminal (assistant-role) step deltas emit unnamed `data: {chunk}`
//!     frames shaped as standard `ChatCompletionChunk` — full OpenAI compat.
//!   - Step boundaries (`step_start`, `step_end`) always emit as `ygg_step`
//!     so the Yggdrasil extension can render fold boundaries / correction
//!     continuation dividers.
//!   - `Done` emits the standard OpenAI `data: [DONE]` terminator.
//!
//! The flow engine owns a `tokio::sync::mpsc::Sender<StreamEvent>` and pushes
//! events as steps progress; the SSE handler owns the receiver and renders
//! each event via [`to_sse_events`].

use axum::response::sse::Event;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::openai::{ChatCompletionChunk, ChunkChoice, Delta, Role};
use crate::proxy::unix_now;

/// Internal protocol between the flow engine and the SSE sink.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "phase")]
pub enum StreamEvent {
    /// A new step has started. `role` signals whether its deltas go to the
    /// main message bubble ("assistant") or the thinking fold ("swarm_thinking").
    #[serde(rename = "step_start")]
    StepStart {
        step: String,
        label: String,
        role: String,
    },
    /// Incremental token(s) emitted by the currently-active step.
    #[serde(rename = "step_delta")]
    StepDelta {
        step: String,
        role: String,
        content: String,
    },
    /// The currently-active step has finished. The engine advances to the
    /// next step or emits `Done`.
    #[serde(rename = "step_end")]
    StepEnd { step: String },
    /// Terminal sentinel — flow execution complete. Always the last event.
    #[serde(rename = "done")]
    Done,
    /// Non-recoverable error mid-stream. Clients should surface and stop.
    #[serde(rename = "error")]
    Error {
        step: Option<String>,
        message: String,
    },
}

/// Construct a bounded mpsc channel sized for typical swarm flows.
pub fn channel() -> (mpsc::Sender<StreamEvent>, mpsc::Receiver<StreamEvent>) {
    mpsc::channel(256)
}

/// Render a `StreamEvent` as one or more SSE `Event` frames.
///
/// - `StepStart` / `StepEnd` / `Error` → `event: ygg_step` + JSON payload.
/// - `StepDelta` with role=="assistant" → unnamed `data: {ChatCompletionChunk}` (standard OpenAI).
/// - `StepDelta` with role!="assistant" → `event: ygg_step` + JSON payload.
/// - `Done` → `data: [DONE]` (standard OpenAI stream terminator).
pub fn to_sse_events(event: &StreamEvent, completion_id: &str, model: &str) -> Vec<Event> {
    let mut out = Vec::new();
    match event {
        StreamEvent::StepStart { .. } | StreamEvent::StepEnd { .. } | StreamEvent::Error { .. } => {
            if let Ok(ev) = Event::default().event("ygg_step").json_data(event) {
                out.push(ev);
            }
        }
        StreamEvent::StepDelta { role, content, .. } => {
            if role == "assistant" {
                let chunk = build_assistant_chunk(completion_id, model, content);
                match serde_json::to_string(&chunk) {
                    Ok(j) => out.push(Event::default().data(j)),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to serialise assistant chunk");
                    }
                }
            } else if let Ok(ev) = Event::default().event("ygg_step").json_data(event) {
                out.push(ev);
            }
        }
        StreamEvent::Done => {
            out.push(Event::default().data("[DONE]"));
        }
    }
    out
}

/// Build a standard OpenAI-compatible `ChatCompletionChunk` carrying `content`.
fn build_assistant_chunk(id: &str, model: &str, content: &str) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: unix_now(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: Some(Role::Assistant),
                content: Some(content.to_string()),
            },
            finish_reason: None,
        }],
    }
}

/// Emit the initial OpenAI "role" chunk (standard first-frame announcement).
/// Called once at the very start of a flow that has any assistant-role step.
pub fn initial_assistant_role_event(completion_id: &str, model: &str) -> Event {
    let chunk = ChatCompletionChunk {
        id: completion_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created: unix_now(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: Some(Role::Assistant),
                content: None,
            },
            finish_reason: None,
        }],
    };
    match serde_json::to_string(&chunk) {
        Ok(j) => Event::default().data(j),
        Err(_) => Event::default().data("{}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_delta_uses_ygg_step_event() {
        let evt = StreamEvent::StepDelta {
            step: "review".into(),
            role: "swarm_thinking".into(),
            content: "Looks ok".into(),
        };
        let events = to_sse_events(&evt, "cmpl-1", "gemma4:e4b");
        assert_eq!(events.len(), 1);
        // Can't introspect Event internals directly; assert it serialises.
        let _ = format!("{events:?}");
    }

    #[test]
    fn assistant_delta_uses_default_data_frame() {
        let evt = StreamEvent::StepDelta {
            step: "draft".into(),
            role: "assistant".into(),
            content: "Hello".into(),
        };
        let events = to_sse_events(&evt, "cmpl-1", "nemotron-3-nano:4b");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn done_emits_openai_terminator() {
        let events = to_sse_events(&StreamEvent::Done, "cmpl-1", "x");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn step_start_and_end_are_ygg_events() {
        let start = StreamEvent::StepStart {
            step: "draft".into(),
            label: "Drafting…".into(),
            role: "assistant".into(),
        };
        let end = StreamEvent::StepEnd {
            step: "draft".into(),
        };
        assert_eq!(to_sse_events(&start, "cmpl-1", "x").len(), 1);
        assert_eq!(to_sse_events(&end, "cmpl-1", "x").len(), 1);
    }
}
