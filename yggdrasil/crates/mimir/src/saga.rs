//! Saga async enrichment for auto-ingested engrams.
//!
//! After the fast cosine classification gate stores an engram, this module provides
//! fire-and-forget enrichment via the Saga LLM (LFM2.5-1.2B-Instruct fine-tuned
//! for memory classification and distillation, running on llama-server).
//!
//! ## Protocol
//!
//! Two tasks are called sequentially for each enrichment:
//!
//! 1. **CLASSIFY**: Verify/correct the category and should_store decision.
//!    If should_store == false, the engram is deleted (Saga veto).
//! 2. **DISTILL**: Extract structured cause/effect/tags from raw content.
//!    Updates the engram in PG with enriched fields.
//!
//! ## Fallback
//!
//! If Saga is unavailable or returns invalid JSON, the engram retains its
//! original classification from the dense cosine gate. No data is lost.

use std::sync::Arc;

use serde::Deserialize;
use uuid::Uuid;

use ygg_domain::config::SagaEnrichConfig;
use ygg_store::postgres::engrams;

use crate::handlers::{engram_content_hash, llm_chat_completion, truncate_to_word_boundary};
use crate::{sdr, state::AppState};

/// Saga CLASSIFY response.
#[derive(Debug, Deserialize)]
struct ClassifyResponse {
    category: String,
    should_store: bool,
    #[serde(default)]
    confidence: f64,
}

/// Saga DISTILL response.
#[derive(Debug, Deserialize)]
struct DistillResponse {
    cause: String,
    effect: String,
    #[serde(default)]
    tags: Vec<String>,
}

/// Wrapper: call shared `llm_chat_completion` with saga config values.
async fn saga_llm_call(
    http: &reqwest::Client,
    cfg: &SagaEnrichConfig,
    prompt: &str,
) -> Result<String, String> {
    llm_chat_completion(http, &cfg.llm_url, &cfg.model, prompt).await
}

/// Strip Qwen3 `<think>...</think>` tags and extract the first JSON object.
fn extract_json(text: &str) -> Option<String> {
    // Strip thinking tags: remove everything between <think> and </think>
    let mut cleaned = text.to_string();
    while let Some(start) = cleaned.find("<think>") {
        if let Some(end) = cleaned.find("</think>") {
            cleaned.replace_range(start..end + "</think>".len(), "");
        } else {
            // Unclosed <think> — strip from <think> to end
            cleaned.truncate(start);
            break;
        }
    }
    let cleaned = cleaned.trim();

    // Extract first JSON object: find matching { ... }
    let start = cleaned.find('{')?;
    let end = cleaned.rfind('}')?;
    if end > start {
        Some(cleaned[start..=end].to_string())
    } else {
        None
    }
}

/// Enrich an auto-ingested engram via Saga CLASSIFY + DISTILL.
///
/// Designed to be called from `tokio::spawn` (fire-and-forget).
/// All errors are logged as warnings, never propagated.
pub async fn enrich_engram(
    state: Arc<AppState>,
    engram_id: Uuid,
    content: String,
    source: String,
    file_path: Option<String>,
    original_category: String,
) {
    let saga_cfg = match state
        .config
        .auto_ingest
        .as_ref()
        .and_then(|c| c.saga.as_ref())
    {
        Some(cfg) if cfg.enabled => cfg.clone(),
        _ => return,
    };

    let http = &state.http_client;

    let file = file_path.as_deref().unwrap_or("");
    let content_truncated: String = content.chars().take(2000).collect();

    // --- Step 1: CLASSIFY ---
    let classify_prompt = format!(
        "CLASSIFY\ntool: {}\nfile: {}\ncontent: {}",
        source, file, &content_truncated
    );

    let mut saga_category = original_category.clone();

    match saga_llm_call(&http, &saga_cfg, &classify_prompt).await {
        Ok(text) => {
            if let Some(json_str) = extract_json(&text) {
                match serde_json::from_str::<ClassifyResponse>(&json_str) {
                    Ok(result) => {
                        if !result.should_store {
                            // Saga veto — delete the engram
                            tracing::info!(
                                engram_id = %engram_id,
                                confidence = result.confidence,
                                "saga vetoed engram (should_store=false)"
                            );
                            if let Err(e) =
                                engrams::delete_engram(state.store.pool(), engram_id).await
                            {
                                tracing::warn!(error = %e, "saga: failed to delete vetoed engram");
                            }
                            // Also remove from in-memory SDR index
                            state.sdr_index.remove(engram_id);
                            return;
                        }
                        if result.category != original_category
                            && result.category != "none"
                            && !result.category.is_empty()
                        {
                            tracing::info!(
                                engram_id = %engram_id,
                                cosine_category = %original_category,
                                saga_category = %result.category,
                                "saga corrected category"
                            );
                            saga_category = result.category;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            raw = %json_str,
                            "saga CLASSIFY JSON parse failed"
                        );
                    }
                }
            } else {
                tracing::warn!(raw = %text, "saga CLASSIFY: no JSON found in response");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "saga CLASSIFY call failed, keeping original");
        }
    }

    // --- Step 2: DISTILL ---
    let distill_prompt = format!(
        "DISTILL\ntool: {}\nfile: {}\ncontent: {}",
        source, file, &content_truncated
    );

    match saga_llm_call(&http, &saga_cfg, &distill_prompt).await {
        Ok(text) => {
            if let Some(json_str) = extract_json(&text) {
                match serde_json::from_str::<DistillResponse>(&json_str) {
                    Ok(result) if result.cause.len() > 5 && result.effect.len() > 5 => {
                        // Re-embed the distilled cause for better SDR representation
                        let embedder = state.embedder.clone();
                        let cause_for_embed = result.cause.clone();
                        let embedding = match tokio::task::spawn_blocking(move || {
                            embedder.embed(&cause_for_embed)
                        })
                        .await
                        {
                            Ok(Ok(emb)) => emb,
                            _ => {
                                tracing::warn!("saga: re-embed failed, keeping original");
                                return;
                            }
                        };

                        let sdr_val = sdr::binarize(&embedding[..sdr::SDR_BITS]);
                        let sdr_bytes = sdr::to_bytes(&sdr_val);
                        let content_hash = engram_content_hash(&result.cause, &result.effect);

                        // Build enriched tags: keep auto_ingest + workstation, use saga category
                        let mut tags: Vec<String> = vec![
                            "auto_ingest".to_string(),
                            saga_category.clone(),
                            "saga_enriched".to_string(),
                        ];
                        for t in &result.tags {
                            if !tags.contains(t) {
                                tags.push(t.clone());
                            }
                        }

                        let trigger_label = truncate_to_word_boundary(&result.cause, 80);

                        let params = ygg_store::postgres::engrams::EngramSdrParams {
                            cause: &result.cause,
                            effect: &result.effect,
                            sdr_bits: &sdr_bytes,
                            content_hash: &content_hash,
                            tags: &tags,
                            trigger_type: "pattern",
                            trigger_label: &trigger_label,
                            project: None,  // saga enrichment preserves original scope
                            scope: "global",
                        };

                        match engrams::update_engram_sdr(
                            state.store.pool(),
                            engram_id,
                            &params,
                        )
                        .await
                        {
                            Ok(true) => {
                                // Update in-memory SDR index with new SDR
                                state.sdr_index.remove(engram_id);
                                state.sdr_index.insert(engram_id, sdr_val);
                                tracing::info!(
                                    engram_id = %engram_id,
                                    category = %saga_category,
                                    "saga enrichment complete"
                                );
                            }
                            Ok(false) => {
                                tracing::warn!(
                                    engram_id = %engram_id,
                                    "saga: engram not found for update (may have been deleted)"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "saga: engram update failed");
                            }
                        }
                    }
                    Ok(_) => {
                        tracing::warn!("saga DISTILL: cause/effect too short, skipping");
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            raw = %json_str,
                            "saga DISTILL JSON parse failed"
                        );
                    }
                }
            } else {
                tracing::warn!(raw = %text, "saga DISTILL: no JSON found in response");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "saga DISTILL call failed, keeping original");
        }
    }
}
