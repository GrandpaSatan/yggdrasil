/// Odin — Yggdrasil LLM Orchestrator
///
/// Module layout:
///   openai        — OpenAI-compatible request/response types and Ollama internal types (leaf, no I/O)
///   error         — OdinError enum with IntoResponse impl
///   router        — Keyword-based semantic router for intent classification
///   memory_router — CALM-inspired zero-injection memory event processor (Sprint 015)
///   state         — Shared AppState passed to all Axum handlers
///   proxy         — Ollama HTTP client: streaming and non-streaming chat, model listing
///   rag           — Parallel context fetch from Muninn + Mimir, system prompt assembly
///   handlers      — Axum route handlers for all public endpoints
///   tool_registry — Static registry of MCP tools for agent loop
///   agent         — Autonomous agent loop for local LLM tool-use
pub mod agent;
pub mod context;
pub mod error;
pub mod handlers;
pub mod memory_router;
pub mod metrics;
pub mod openai;
pub mod proxy;
pub mod rag;
pub mod router;
pub mod session;
pub mod state;
pub mod tool_registry;
pub mod voice_ws;
