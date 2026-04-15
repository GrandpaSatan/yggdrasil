//! Shared bearer-token auth middleware for Yggdrasil HTTP services.
//!
//! Sprint 069 Phase C (VULN-001): Odin, Mimir, and Muninn gain a uniform
//! `Authorization: Bearer <token>` check on every request, with a small
//! allowlist of exempt public paths (`/health`, `/metrics`).
//!
//! Token source: `YGG_SERVICE_TOKEN` env var (falls back to
//! `MIMIR_VAULT_CLIENT_TOKEN` for backwards compatibility with Sprint 064's
//! vault-auth token so a single token secures the whole fleet).
//!
//! Exemptions (in addition to `PUBLIC_PATHS`):
//!   - Requests bearing `X-Yggdrasil-Internal: true` header (Sprint 068
//!     pattern for node-to-node traffic) — dreamer's warmup + flow_runner
//!     calls are set this way and must not hit the bearer check.
//!
//! Disable path (dev only): set `YGG_AUTH_DISABLE=1`. A WARN log fires once
//! at startup so the condition can't silently escape into production.

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, Response, StatusCode};
use axum::middleware::Next;

/// Paths that are ALWAYS reachable without a Bearer token. Matched as
/// exact strings (no prefix matching — `/metrics/foo` is not exempt).
///
/// `/api/v1/mesh/hello` is public because the mesh handshake is its own
/// auth layer (VULN-006 adds a pre-shared key + content forgery checks
/// inside the handler). Bearer auth would be redundant here, and peer
/// nodes attempting the initial handshake don't have the service token.
pub const PUBLIC_PATHS: &[&str] = &["/health", "/metrics", "/api/v1/mesh/hello"];

const SERVICE_TOKEN_ENV: &str = "YGG_SERVICE_TOKEN";
const FALLBACK_TOKEN_ENV: &str = "MIMIR_VAULT_CLIENT_TOKEN";
const DISABLE_ENV: &str = "YGG_AUTH_DISABLE";
const INTERNAL_HEADER: &str = "x-yggdrasil-internal";

/// Resolve the expected bearer token from env. Returns `None` when auth is
/// disabled or no token is configured, in which case every request passes.
/// This preserves dev ergonomics — set `YGG_SERVICE_TOKEN` to opt in.
pub fn configured_token() -> Option<String> {
    if std::env::var(DISABLE_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        tracing::warn!(
            "YGG_AUTH_DISABLE=1 — bearer auth middleware is a no-op. DEV ONLY."
        );
        return None;
    }
    std::env::var(SERVICE_TOKEN_ENV)
        .ok()
        .or_else(|| std::env::var(FALLBACK_TOKEN_ENV).ok())
        .filter(|s| !s.is_empty())
}

/// Constant-time equality for token comparison. Mirrors mimir::vault::ct_eq.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn has_internal_header(headers: &HeaderMap) -> bool {
    headers
        .get(INTERNAL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn reject_401(msg: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::WWW_AUTHENTICATE, "Bearer realm=\"yggdrasil\"")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!("{{\"error\":\"{msg}\"}}")))
        .expect("static response should build")
}

/// Axum middleware that enforces Bearer auth on all non-public routes.
///
/// Install with
/// ```ignore
/// .layer(middleware::from_fn_with_state(
///     ygg_server::auth::configured_token(),
///     ygg_server::auth::bearer_auth,
/// ))
/// ```
/// or plain `middleware::from_fn(ygg_server::auth::bearer_auth)` — the
/// middleware re-resolves `configured_token()` on each call so a missing
/// env leaves auth disabled.
pub async fn bearer_auth(req: Request<Body>, next: Next) -> Response<Body> {
    // Public paths (/health, /metrics) bypass the check entirely.
    let path = req.uri().path();
    if PUBLIC_PATHS.iter().any(|p| *p == path) {
        return next.run(req).await;
    }

    // Internal node-to-node calls (dreamer warmup, flow runners) bypass via
    // the Sprint 068 Phase 6a header convention.
    if has_internal_header(req.headers()) {
        return next.run(req).await;
    }

    let Some(expected) = configured_token() else {
        // Auth unconfigured — permit. Dev path. The startup WARN above
        // catches the typical "prod deployed without a token" mistake.
        return next.run(req).await;
    };

    let header_val = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let Some(header_val) = header_val else {
        return reject_401("missing Authorization header");
    };
    let Some(token) = header_val.strip_prefix("Bearer ") else {
        return reject_401("Authorization scheme must be Bearer");
    };
    if !ct_eq(token.as_bytes(), expected.as_bytes()) {
        return reject_401("invalid bearer token");
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_is_length_sensitive() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn ct_eq_rejects_mismatch_without_leaking_prefix() {
        assert!(!ct_eq(b"abcdef", b"abcxxx"));
    }

    #[test]
    fn public_paths_contain_health_and_metrics() {
        assert!(PUBLIC_PATHS.contains(&"/health"));
        assert!(PUBLIC_PATHS.contains(&"/metrics"));
    }

    #[test]
    fn has_internal_header_parses_true_variants() {
        let mut h = HeaderMap::new();
        h.insert(INTERNAL_HEADER, "true".parse().unwrap());
        assert!(has_internal_header(&h));
        h.insert(INTERNAL_HEADER, "TRUE".parse().unwrap());
        assert!(has_internal_header(&h));
        h.insert(INTERNAL_HEADER, "false".parse().unwrap());
        assert!(!has_internal_header(&h));
        h.clear();
        assert!(!has_internal_header(&h));
    }
}
