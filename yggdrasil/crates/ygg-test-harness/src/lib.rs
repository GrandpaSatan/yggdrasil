//! Shared test harness for Yggdrasil integration tests.
//!
//! Provides mock HTTP server builders for Ollama, Mimir, and Muninn,
//! pre-built response fixtures, circuit breaker test helpers, and
//! flow assertion utilities for multi-step pipeline E2E tests.

pub mod mocks;
pub mod fixtures;
pub mod circuit_breaker;
pub mod flow_assertions;

pub use mocks::{MockMimir, MockMimirBuilder, MockMuninn, MockMuninnBuilder, MockOllama, MockOllamaBuilder};
pub use flow_assertions::{
    assert_content_contains_any,
    assert_content_contains_any_json,
    assert_flow_executed_json,
    non_empty_choice_count,
};
