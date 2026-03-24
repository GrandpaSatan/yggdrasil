//! Shared HTTP service boilerplate for Yggdrasil services.
//!
//! Consolidates duplicated infrastructure across Odin, Mimir, Muninn, Huginn,
//! and ygg-node: metrics middleware, graceful shutdown, error types, health
//! checks, telemetry initialization, and systemd notification.

pub mod error;
pub mod health;
pub mod init;
pub mod metrics;
pub mod shutdown;
