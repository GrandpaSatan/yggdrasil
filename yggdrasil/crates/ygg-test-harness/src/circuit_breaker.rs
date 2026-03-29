//! Circuit breaker test helpers — servers that fail on demand.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

/// Start a mock server that always returns the given HTTP status code.
pub async fn start_failing_server(status: u16) -> String {
    let code = StatusCode::from_u16(status).expect("valid status code");
    let app = Router::new().route(
        "/{*path}",
        post(move || async move { code.into_response() }),
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind failing server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://127.0.0.1:{}", addr.port())
}

/// Start a mock server that returns `fail_status` for the first `fail_count`
/// requests, then returns 200 with `{"ok": true}` for all subsequent requests.
pub async fn start_flaky_server(fail_count: u32, fail_status: u16) -> String {
    let counter = Arc::new(AtomicU32::new(0));
    let fail_code = StatusCode::from_u16(fail_status).expect("valid status code");

    let app = Router::new().route(
        "/{*path}",
        post({
            let counter = counter.clone();
            move || {
                let counter = counter.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::Relaxed);
                    if n < fail_count {
                        fail_code.into_response()
                    } else {
                        (StatusCode::OK, "{\"ok\": true}").into_response()
                    }
                }
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind flaky server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://127.0.0.1:{}", addr.port())
}
