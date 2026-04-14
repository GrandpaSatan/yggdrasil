//! ygg-dreamer — Yggdrasil "Always-On" dream mode daemon (Sprint 065 Track C).
//!
//! Consolidates two previously-fragmented pieces of infrastructure:
//!
//! 1. **Sprint 055 Background Flow Scheduler** — 360 lines of idle+cron
//!    dispatch logic in `crates/odin/src/flow_scheduler.rs` that was never
//!    spawned in Odin's `main.rs`. Dead code since 2026-03-14. This crate
//!    revives it by copying the pure scheduling helpers and wrapping them
//!    in an HTTP-polling loop against Odin.
//!
//! 2. **Sprint 064 P2 keep_warm injector** — embedded in Odin at
//!    `crates/odin/src/keep_warm.rs`. Functional but coupled to Odin's
//!    http_client. The dreamer warmup loop subsumes the same role with
//!    a wider prefix catalog (swarm drafter / dream exploration / HA
//!    intent) primed specifically to populate the vLLM LMCache disk tier
//!    once Track B lands.
//!
//! Deployment target: Munin. Talks to Odin via `http://10.0.65.8:8080`
//! (consumes `/internal/activity` for idle detection) and Mimir via
//! `http://10.0.65.8:9090/api/v1/store` (persists dream engrams tagged
//! `dreamer sprint:NNN`).

pub mod config;
pub mod flow_runner;
pub mod scheduler;
pub mod warmup;
