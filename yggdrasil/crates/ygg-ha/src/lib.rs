//! Home Assistant integration client for Yggdrasil.
//!
//! Provides a REST API client for the HA instance running on chirp (REDACTED_CHIRP_IP).
//! Exposes entity queries, service calls, and automation YAML generation.

pub mod automation;
pub mod client;
pub mod error;

pub use automation::AutomationGenerator;
pub use client::{DomainServices, EntityState, HaClient, ServiceDef};
pub use error::HaError;
