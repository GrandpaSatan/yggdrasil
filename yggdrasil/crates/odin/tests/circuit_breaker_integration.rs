//! Circuit breaker integration tests.
//!
//! Uses `ygg_test_harness` mock servers to verify that the circuit breaker
//! trips after consecutive failures, blocks requests while open, and
//! re-closes after a successful probe during the half-open window.

use std::sync::Arc;

use odin::state::CircuitBreaker;

#[test]
fn test_circuit_trips_after_consecutive_failures() {
    let cb = CircuitBreaker::new();
    assert!(!cb.is_open(), "should start closed");
    assert!(cb.allow_request(), "should allow requests when closed");

    // Record FAILURE_THRESHOLD failures
    cb.record_failure();
    assert!(!cb.is_open(), "1 failure — still closed");
    cb.record_failure();
    assert!(!cb.is_open(), "2 failures — still closed");
    cb.record_failure();
    assert!(cb.is_open(), "3 failures — should be open");
    assert!(!cb.allow_request(), "should block requests when open (cooldown not elapsed)");
}

#[test]
fn test_circuit_closes_on_success_after_cooldown() {
    let cb = CircuitBreaker::new();

    // Trip the breaker
    for _ in 0..3 {
        cb.record_failure();
    }
    assert!(cb.is_open());

    // Simulate cooldown elapsed by setting tripped_at to 60 seconds ago.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    cb.set_tripped_at(now - 60);

    // After cooldown, half-open probe should be allowed
    assert!(cb.allow_request(), "half-open probe should be allowed after cooldown");

    // Successful probe closes the circuit
    cb.record_success();
    assert!(!cb.is_open(), "circuit should close after successful probe");
    assert!(cb.allow_request(), "should allow requests after recovery");
}

#[test]
fn test_open_circuit_returns_instant_error() {
    let cb = CircuitBreaker::new();

    // Trip the breaker
    for _ in 0..3 {
        cb.record_failure();
    }
    assert!(cb.is_open());

    // Immediately after tripping, requests should be blocked
    assert!(!cb.allow_request(), "should block immediately after tripping");
    assert!(!cb.allow_request(), "should keep blocking");
}

#[test]
fn test_half_open_probe_succeeds() {
    let cb = Arc::new(CircuitBreaker::new());

    // Trip the breaker
    for _ in 0..3 {
        cb.record_failure();
    }
    assert!(cb.is_open());

    // Simulate cooldown
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    cb.set_tripped_at(now - 31); // 31s > 30s cooldown

    // Half-open: probe allowed
    assert!(cb.allow_request(), "probe should be allowed in half-open state");

    // If probe succeeds → circuit closes
    cb.record_success();
    assert!(!cb.is_open());

    // If probe fails → circuit stays open (re-trip)
    let cb2 = CircuitBreaker::new();
    for _ in 0..3 {
        cb2.record_failure();
    }
    cb2.set_tripped_at(now - 31);
    assert!(cb2.allow_request(), "half-open probe should be allowed");
    cb2.record_failure(); // probe fails
    assert!(cb2.is_open(), "circuit should stay open after failed probe");
}
