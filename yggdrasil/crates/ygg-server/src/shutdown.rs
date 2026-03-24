//! Graceful shutdown signal handler.
//!
//! Listens for CTRL+C and SIGTERM (on Unix), completing when either fires.
//! Replaces four identical `shutdown_signal()` implementations across services.

/// Wait for a shutdown signal (CTRL+C or SIGTERM on Unix).
///
/// Use with `axum::serve(...).with_graceful_shutdown(ygg_server::shutdown::signal())`.
pub async fn signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install CTRL+C signal handler");
        }
    };

    #[cfg(unix)]
    let sigterm = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to install SIGTERM signal handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received CTRL+C, shutting down"),
        () = sigterm => tracing::info!("received SIGTERM, shutting down"),
    }
}
