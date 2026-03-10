/// RAG (Retrieval-Augmented Generation) context fetching and prompt assembly.
///
/// This module is the only place in Odin that communicates with Muninn
/// (`/api/v1/search`) and Mimir (`/api/v1/recall`) for context retrieval.
///
/// Both calls are best-effort with a 3-second timeout each.  If either
/// service is unreachable or times out, the corresponding context is `None`
/// and chat completion proceeds without it.  Failures are logged as warnings,
/// not propagated as errors.
///
/// Muninn and Mimir are queried in parallel via `tokio::join!` so the total
/// latency is `max(muninn_latency, mimir_latency)` rather than their sum.
///
/// ## HA-aware behaviour (Sprint 007)
///
/// When `intent == "home_assistant"` or `intent == "home_automation"`:
/// - Muninn code context fetch is **skipped** (code search irrelevant for HA
///   queries and would add 200-500ms latency).
/// - Mimir memory-event fetch is **kept** (prior HA interactions influence
///   routing confidence).
/// - HA domain summary is **injected** into the system prompt using a 60-second
///   in-memory cache (`AppState.ha_context_cache`) to avoid hammering the HA
///   REST API on every message.
///
/// ## Sprint 015: Zero-injection memory architecture
///
/// Memory events from Mimir are stored in `RagContext.memory_events` as
/// structured `RecallResponse` values.  They influence routing via
/// `memory_router::apply_memory_events` but **no engram text enters the LLM
/// prompt**.  The `engram_context` formatted-string field is removed.
use serde::Deserialize;
use tokio::time::Instant;

use ygg_domain::engram::{RecallQuery, RecallResponse};

use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────

/// Assembled RAG context from all retrieval sources.
///
/// `memory_events` replaces the old `engram_context: Option<String>`.
/// Memory events are never serialized into the system prompt; they are
/// consumed by `memory_router::apply_memory_events` to influence routing.
#[derive(Debug, Default)]
pub struct RagContext {
    /// Code context string assembled by Muninn from indexed chunks.
    /// `None` when intent is `home_assistant`/`home_automation`.
    pub code_context: Option<String>,
    /// Structured memory events from Mimir (Sprint 015).
    /// No text from these events enters the LLM prompt — zero injection.
    pub memory_events: Option<RecallResponse>,
    /// HA domain summary for `home_assistant`/`home_automation` intents.
    /// Injected into the system prompt when present.
    pub ha_context: Option<String>,
}

// ─────────────────────────────────────────────────────────────────
// Internal deserialization types
// ─────────────────────────────────────────────────────────────────

/// Minimal view of Muninn's `SearchResponse` — only the assembled context
/// string is needed; individual result fields are ignored.
#[derive(Deserialize)]
struct MuninnSearchResponse {
    context: String,
}

// ─────────────────────────────────────────────────────────────────
// Context fetching
// ─────────────────────────────────────────────────────────────────

/// HA intents — both the legacy name and the Sprint 007 spec name.
fn is_ha_intent(intent: &str) -> bool {
    intent == "home_assistant" || intent == "home_automation"
}

/// Fetch RAG context from Muninn (code) and Mimir (memory events) in parallel.
///
/// `intent` controls HA-aware behaviour:
/// - `"home_assistant"` / `"home_automation"`: skip Muninn, keep Mimir recall,
///   inject HA domain summary from the 60s cache.
/// - Any other intent: fetch both Muninn and Mimir as before.
///
/// Failures on either leg are swallowed and logged so that chat completion
/// remains available even when retrieval services are down.
///
/// ## Sprint 015: Zero-injection memory
///
/// Mimir is now queried via `POST /api/v1/recall` (not `/api/v1/query`).
/// The returned `RecallResponse` contains structured events — no cause/effect
/// text.  The caller (`handlers::chat_handler`) passes `memory_events` to
/// `memory_router::apply_memory_events` to refine routing, not to the prompt.
pub async fn fetch_context(state: &AppState, query: &str, intent: &str) -> RagContext {
    let fetch_muninn = !is_ha_intent(intent);

    // Fire parallel fetches (Muninn is skipped for HA intents).
    let (code_result, memory_result) = tokio::join!(
        async {
            if fetch_muninn {
                fetch_code_context(
                    &state.http_client,
                    &state.muninn_url,
                    query,
                    state.config.muninn.max_context_chunks,
                )
                .await
            } else {
                tracing::debug!(intent = intent, "skipping muninn code context for HA intent");
                None
            }
        },
        fetch_memory_events(
            &state.http_client,
            &state.mimir_url,
            query,
            state.config.mimir.query_limit,
        ),
    );

    // For HA intents, inject a domain summary from the HA instance.
    let ha_context = if is_ha_intent(intent) {
        fetch_ha_domain_summary(state).await
    } else {
        None
    };

    RagContext {
        code_context: code_result,
        memory_events: memory_result,
        ha_context,
    }
}

/// Fetch the HA domain summary, using a 60-second in-memory cache.
///
/// Returns `None` if HA is not configured or the HA instance is unreachable.
/// The cache is a `RwLock<Option<(Instant, String)>>` stored in `AppState`.
async fn fetch_ha_domain_summary(state: &AppState) -> Option<String> {
    let ha_client = state.ha_client.as_ref()?;

    const CACHE_TTL_SECS: u64 = 60;

    // ── Fast path: check cache under read lock ────────────────────
    {
        let guard = state.ha_context_cache.read().await;
        if let Some((cached_at, ref summary)) = *guard {
            if cached_at.elapsed().as_secs() < CACHE_TTL_SECS {
                tracing::debug!("using cached HA domain summary");
                return Some(summary.clone());
            }
        }
    }

    // ── Slow path: refresh cache under write lock ─────────────────
    // Re-check under write lock to avoid a thundering-herd refresh.
    let mut guard = state.ha_context_cache.write().await;
    if let Some((cached_at, ref summary)) = *guard {
        if cached_at.elapsed().as_secs() < CACHE_TTL_SECS {
            return Some(summary.clone());
        }
    }

    // Fetch fresh data from HA.
    let states = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ha_client.get_states(),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "failed to fetch HA states for context injection");
            return None;
        }
        Err(_) => {
            tracing::warn!("HA get_states timed out after 5s for context injection");
            return None;
        }
    };

    // Build domain summary: domain name + entity count.
    let summary = build_ha_domain_summary(&states);
    tracing::debug!(domains = summary.lines().count(), "HA domain summary refreshed");

    *guard = Some((Instant::now(), summary.clone()));
    Some(summary)
}

/// Build the HA domain summary string injected into the system prompt.
fn build_ha_domain_summary(states: &[ygg_ha::EntityState]) -> String {
    use std::collections::BTreeMap;

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for state in states {
        let domain = state
            .entity_id
            .split_once('.')
            .map(|(d, _)| d)
            .unwrap_or("unknown");
        *counts.entry(domain).or_default() += 1;
    }

    let mut out = String::new();
    for (domain, count) in &counts {
        out.push_str(&format!("- {}: {} entities\n", domain, count));
    }
    out
}

/// Truncate a query string to at most `max_chars` on a word boundary.
///
/// Embedding models (qwen3-embedding) have limited context windows.  Clients
/// like Roo Code send huge messages with `<environment_details>` blocks
/// containing hundreds of file paths.  Truncating before the embed call
/// prevents 500 "input length exceeds context length" errors from Ollama.
fn truncate_query(query: &str, max_chars: usize) -> &str {
    if query.len() <= max_chars {
        return query;
    }
    // Try to cut on a word boundary.
    match query[..max_chars].rfind(char::is_whitespace) {
        Some(pos) => &query[..pos],
        None => &query[..max_chars],
    }
}

/// Query Muninn for code context.
///
/// Sends `POST /api/v1/search` with a 3-second timeout and extracts the
/// pre-assembled `context` string from the response.  Returns `None` on any
/// error or timeout.
async fn fetch_code_context(
    client: &reqwest::Client,
    muninn_url: &str,
    query: &str,
    limit: usize,
) -> Option<String> {
    let url = format!("{muninn_url}/api/v1/search");
    let query = truncate_query(query, 512);
    let body = serde_json::json!({ "query": query, "limit": limit });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        client.post(&url).json(&body).send(),
    )
    .await;

    let response = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(url = %url, error = %e, "muninn search request failed");
            return None;
        }
        Err(_) => {
            tracing::warn!(url = %url, "muninn search timed out after 3s");
            return None;
        }
    };

    if !response.status().is_success() {
        tracing::warn!(url = %url, status = %response.status(), "muninn search returned non-success status");
        return None;
    }

    match response.json::<MuninnSearchResponse>().await {
        Ok(search_resp) if !search_resp.context.is_empty() => {
            tracing::debug!(chars = search_resp.context.len(), "code context fetched from muninn");
            Some(search_resp.context)
        }
        Ok(_) => {
            // Empty context — no matching code chunks.
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse muninn search response");
            None
        }
    }
}

/// Fetch memory events from Mimir (Sprint 015 zero-injection path).
///
/// Sends `POST /api/v1/recall` with a 3-second timeout.  Returns a
/// `RecallResponse` containing structured `EngramEvent` values — **no**
/// cause/effect text.  Returns `None` on any error, timeout, or non-success
/// HTTP status.
///
/// The caller is responsible for consuming these events through
/// `memory_router::apply_memory_events`; they must never be serialized into
/// the LLM system prompt.
pub async fn fetch_memory_events(
    client: &reqwest::Client,
    mimir_url: &str,
    query: &str,
    limit: usize,
) -> Option<RecallResponse> {
    let url = format!("{mimir_url}/api/v1/recall");
    let query = truncate_query(query, 512);
    let body = RecallQuery {
        text: query.to_string(),
        limit,
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        client.post(&url).json(&body).send(),
    )
    .await;

    let response = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(url = %url, error = %e, "mimir recall request failed");
            return None;
        }
        Err(_) => {
            tracing::warn!(url = %url, "mimir recall timed out after 3s");
            return None;
        }
    };

    if !response.status().is_success() {
        tracing::warn!(
            url = %url,
            status = %response.status(),
            "mimir recall returned non-success status"
        );
        return None;
    }

    match response.json::<RecallResponse>().await {
        Ok(recall) => {
            tracing::debug!(
                events = recall.events.len(),
                core_events = recall.core_events.len(),
                "memory events fetched from mimir recall"
            );
            Some(recall)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse mimir recall response");
            None
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Prompt assembly
// ─────────────────────────────────────────────────────────────────

/// Build the system prompt content, optionally appending RAG context.
///
/// The base prompt is static per invocation.  Only two sources of external
/// text are permitted in the prompt:
/// - Muninn code context (verbatim code chunks from the indexed codebase).
/// - HA domain summary (entity domain counts from Home Assistant).
///
/// ## Sprint 015: Zero memory-text injection
///
/// Memory events (`RagContext.memory_events`) are deliberately **excluded**
/// from this function.  They influence routing structurally via
/// `memory_router::apply_memory_events` but must never appear as prompt text.
/// The old `## Relevant Memories` block is permanently removed.
#[must_use]
pub fn build_system_prompt(rag: &RagContext) -> String {
    let mut prompt = String::from(
        "You are a helpful AI assistant with access to a codebase.\n\
         Answer questions accurately and concisely. When discussing code, reference specific files \
         and line numbers when available.",
    );

    if let Some(code_ctx) = &rag.code_context {
        prompt.push_str("\n\n## Relevant Code Context\n\n");
        prompt.push_str(code_ctx);
    }

    // NOTE: memory_events are intentionally NOT rendered here.
    // See memory_router::apply_memory_events for their structural use.

    if let Some(ha_ctx) = &rag.ha_context {
        prompt.push_str(
            "\n\n## Home Assistant Context\n\
             You have access to a Home Assistant instance with the following entity domains:\n",
        );
        prompt.push_str(ha_ctx);
        prompt.push_str(
            "\nYou can reference specific entities by their entity_id (e.g., light.living_room).\n\
             When the user asks about home automation, provide specific entity IDs and service \
             calls.",
        );
    }

    prompt
}
