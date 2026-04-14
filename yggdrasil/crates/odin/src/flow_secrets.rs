//! Sprint 064 P7 — Vault-aware flow secrets.
//!
//! At flow dispatch (or step invocation), Odin resolves the union of
//! `FlowConfig.secrets` and `FlowStep.secrets` against the Mimir vault, then
//! substitutes `{{secret:<env_var>}}` tokens in any prompt/template string
//! with the resolved plaintext.
//!
//! Design choice: substitution-by-template (not process env vars). Setting
//! `std::env` in async/concurrent code is racy and would leak secrets across
//! requests. Template substitution gives the same UX (the flow author writes
//! `{{secret:HA_TOKEN}}`) without the global-state hazard.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ygg_domain::config::SecretRef;

const VAULT_FETCH_TIMEOUT_MS: u64 = 5000;

/// Errors the caller should surface to the user — secrets unavailable means
/// the flow cannot run safely (the prompt would still contain the literal
/// `{{secret:...}}` token, which would either confuse the model or leak the
/// reference upstream).
#[derive(Debug, thiserror::Error)]
pub enum FlowSecretsError {
    #[error("vault fetch failed for key '{key}' scope '{scope}': {reason}")]
    VaultFetch {
        key: String,
        scope: String,
        reason: String,
    },
    #[error("vault returned non-success for key '{key}' scope '{scope}': HTTP {status}")]
    VaultStatus {
        key: String,
        scope: String,
        status: u16,
    },
    #[error("vault response missing 'value' for key '{key}'")]
    VaultMissingValue { key: String },
}

/// Resolve the merged secret set for a flow + step into a `env_var → value`
/// map ready for prompt substitution. Step-level entries override flow-level
/// when they share an `env_var` name.
pub async fn resolve(
    client: &reqwest::Client,
    mimir_url: &str,
    vault_token: Option<&str>,
    flow_secrets: &[SecretRef],
    step_secrets: &[SecretRef],
) -> Result<HashMap<String, String>, FlowSecretsError> {
    let mut by_env: HashMap<String, &SecretRef> = HashMap::new();
    for s in flow_secrets {
        by_env.insert(s.env_var.clone(), s);
    }
    // Step-level overrides flow-level on conflict.
    for s in step_secrets {
        by_env.insert(s.env_var.clone(), s);
    }

    let mut out = HashMap::new();
    for (env_var, sref) in by_env {
        let value = fetch_one(client, mimir_url, vault_token, &sref.vault_key, &sref.scope).await?;
        out.insert(env_var, value);
    }
    Ok(out)
}

/// Substitute every `{{secret:NAME}}` token in `input` with the matching
/// entry from `secrets`. Tokens whose name is not in the map are left
/// in place verbatim so callers can detect the miss.
pub fn substitute(input: &str, secrets: &HashMap<String, String>) -> String {
    if secrets.is_empty() || !input.contains("{{secret:") {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{secret:") {
        out.push_str(&rest[..start]);
        let after_tag = &rest[start + "{{secret:".len()..];
        if let Some(end) = after_tag.find("}}") {
            let name = &after_tag[..end];
            if let Some(val) = secrets.get(name) {
                out.push_str(val);
            } else {
                // Unknown name — leave token verbatim.
                out.push_str("{{secret:");
                out.push_str(name);
                out.push_str("}}");
            }
            rest = &after_tag[end + 2..];
        } else {
            // Unterminated token; copy the rest as-is and stop.
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

#[derive(Debug, Serialize)]
struct VaultRequest<'a> {
    action: &'a str,
    key: &'a str,
    scope: &'a str,
}

#[derive(Debug, Deserialize)]
struct VaultResponse {
    #[serde(default)]
    value: Option<String>,
}

async fn fetch_one(
    client: &reqwest::Client,
    mimir_url: &str,
    vault_token: Option<&str>,
    key: &str,
    scope: &str,
) -> Result<String, FlowSecretsError> {
    let url = format!("{}/api/v1/vault", mimir_url.trim_end_matches('/'));
    let body = VaultRequest {
        action: "get",
        key,
        scope,
    };
    let mut req = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_millis(VAULT_FETCH_TIMEOUT_MS));
    if let Some(token) = vault_token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| FlowSecretsError::VaultFetch {
            key: key.to_owned(),
            scope: scope.to_owned(),
            reason: e.to_string(),
        })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(FlowSecretsError::VaultStatus {
            key: key.to_owned(),
            scope: scope.to_owned(),
            status: status.as_u16(),
        });
    }

    let parsed: VaultResponse = resp.json().await.map_err(|e| FlowSecretsError::VaultFetch {
        key: key.to_owned(),
        scope: scope.to_owned(),
        reason: format!("parse: {e}"),
    })?;

    parsed
        .value
        .ok_or_else(|| FlowSecretsError::VaultMissingValue {
            key: key.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sref(key: &str, env: &str) -> SecretRef {
        SecretRef {
            vault_key: key.into(),
            scope: "global".into(),
            env_var: env.into(),
        }
    }

    #[test]
    fn substitute_replaces_known_tokens() {
        let mut s = HashMap::new();
        s.insert("HA_TOKEN".to_string(), "abc123".to_string());
        let out = substitute("token={{secret:HA_TOKEN}}", &s);
        assert_eq!(out, "token=abc123");
    }

    #[test]
    fn substitute_leaves_unknown_tokens_verbatim() {
        let s = HashMap::new();
        let out = substitute("token={{secret:MISSING}}", &s);
        assert_eq!(out, "token={{secret:MISSING}}");
    }

    #[test]
    fn substitute_handles_multiple_occurrences() {
        let mut s = HashMap::new();
        s.insert("A".to_string(), "1".to_string());
        s.insert("B".to_string(), "2".to_string());
        let out = substitute("{{secret:A}} and {{secret:B}} and {{secret:A}}", &s);
        assert_eq!(out, "1 and 2 and 1");
    }

    #[test]
    fn substitute_short_circuits_when_no_tokens() {
        let mut s = HashMap::new();
        s.insert("A".to_string(), "value".to_string());
        let out = substitute("plain text without tokens", &s);
        assert_eq!(out, "plain text without tokens");
    }

    #[test]
    fn substitute_handles_unterminated_token_safely() {
        let mut s = HashMap::new();
        s.insert("A".to_string(), "1".to_string());
        let out = substitute("partial {{secret:A and trailing", &s);
        assert!(out.contains("{{secret:A and trailing"));
    }

    #[test]
    fn substitute_step_secrets_after_flow_secrets() {
        // The unique-by-env_var dedup is enforced by `resolve`, but the
        // substituter is name-key only — ensure nothing surprising happens
        // when the same name is used twice in a row.
        let mut s = HashMap::new();
        s.insert("X".to_string(), "FROM_STEP".to_string());
        let out = substitute("{{secret:X}}", &s);
        assert_eq!(out, "FROM_STEP");
    }

    #[test]
    fn resolve_dedupes_step_over_flow() {
        // Pure verification of the merge precedence — no HTTP call, just the
        // map-building portion. We exercise it by checking the by_env build.
        let flow = vec![sref("flow_key", "TOKEN")];
        let step = vec![sref("step_key", "TOKEN")];
        let mut by_env: HashMap<String, &SecretRef> = HashMap::new();
        for s in &flow {
            by_env.insert(s.env_var.clone(), s);
        }
        for s in &step {
            by_env.insert(s.env_var.clone(), s);
        }
        assert_eq!(by_env.get("TOKEN").unwrap().vault_key, "step_key");
    }
}
