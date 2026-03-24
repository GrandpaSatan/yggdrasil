//! Telemetry initialization and systemd notification helpers.
//!
//! Consolidates the identical tracing + Prometheus setup duplicated across
//! all Yggdrasil services.

use metrics_exporter_prometheus::PrometheusHandle;
use tracing_subscriber::EnvFilter;

/// Initialize tracing with env-filter and install the Prometheus metrics
/// recorder. Returns the `PrometheusHandle` for exposing `/metrics`.
///
/// Call once at the top of `main()` before any other work.
///
/// # Panics
///
/// Panics if the tracing subscriber or Prometheus recorder cannot be
/// installed (typically means another subscriber/recorder is already set).
pub fn telemetry() -> PrometheusHandle {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}

/// Notify systemd that the service is ready (Type=notify services).
///
/// Silently ignored if `sd_notify` is not available (e.g. running outside
/// systemd).
pub fn sd_ready() {
    let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);
}
