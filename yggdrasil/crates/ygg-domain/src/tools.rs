//! Canonical tool catalog for the Yggdrasil ecosystem.
//!
//! This module is the **single source of truth** for tool metadata: names,
//! descriptions, safety tiers, voice keywords, and timeout overrides.
//!
//! Both Odin's agent `tool_registry` and the MCP `server.rs` consume this
//! catalog so that tool definitions stay in sync.  Endpoint routing (Mimir
//! vs Muninn vs OdinSelf vs HA) remains in the respective crates because
//! it is deployment-specific.
//!
//! ## Adding a new tool
//! 1. Add a `ToolMeta` entry to `ALL_TOOLS` below.
//! 2. Add the param struct here (with `#[derive(JsonSchema)]`).
//! 3. Wire the endpoint in `odin::tool_registry` and/or `ygg-mcp::server`.

use serde::{Deserialize, Serialize};

// Re-export schemars for consumers that need schema_for::<T>().
pub use schemars;
pub use schemars::JsonSchema;

// ─────────────────────────────────────────────────────────────────
// Tool metadata
// ─────────────────────────────────────────────────────────────────

/// Safety tier controlling which tools an LLM agent may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolTier {
    /// Read-only, always allowed.
    Safe,
    /// Write operations, require explicit opt-in.
    Restricted,
    /// Never allowed for LLM agents.
    Blocked,
}

/// Static metadata for a single tool.  Lives in the catalog; does NOT
/// include endpoint routing (which is crate-specific).
#[derive(Debug, Clone)]
pub struct ToolMeta {
    /// Unique tool name (matches MCP tool name without `_tool` suffix).
    pub name: &'static str,
    /// Human-readable description shown to the LLM.
    pub description: &'static str,
    /// Safety tier.
    pub tier: ToolTier,
    /// Per-tool timeout override in seconds.  `None` → use global default.
    pub timeout_override_secs: Option<u64>,
    /// Keyword triggers for voice query-based tool selection.
    pub keywords: &'static [&'static str],
    /// Always include in keyword-based selection regardless of query.
    pub voice_always: bool,
}

/// The canonical tool catalog.  Every tool in the ecosystem MUST have an
/// entry here.  Order is cosmetic (safe first, then restricted).
pub static ALL_TOOLS: &[ToolMeta] = &[
    // ── Safe tier (read-only) ────────────────────────────────────
    ToolMeta {
        name: "search_code",
        description: "Search the codebase using semantic and keyword search. Returns matching code chunks with file paths and line numbers.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["code", "function", "codebase", "implementation", "source", "module"],
        voice_always: false,
    },
    ToolMeta {
        name: "query_memory",
        description: "Search engram memory for relevant past context. Returns cause/effect pairs with similarity scores.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["remember", "recall", "memory", "previously", "last time", "did we"],
        voice_always: true,
    },
    ToolMeta {
        name: "memory_intersect",
        description: "Find semantic intersection of multiple texts using SDR operations.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "get_sprint_history",
        description: "Retrieve archived sprint documents for a project.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["sprint"],
        voice_always: false,
    },
    ToolMeta {
        name: "memory_timeline",
        description: "Query memory events within a time range.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["timeline", "history", "when did"],
        voice_always: false,
    },
    ToolMeta {
        name: "list_models",
        description: "List all LLM models available through Odin.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["model", "models", "llm", "available models"],
        voice_always: false,
    },
    ToolMeta {
        name: "service_health",
        description: "Check health status of Yggdrasil services.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["health", "status", "service", "running", "online"],
        voice_always: false,
    },
    ToolMeta {
        name: "ast_analyze",
        description: "Look up code symbols (functions, structs, traits) using AST analysis.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "impact_analysis",
        description: "Find all references to a symbol across the codebase.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "ha_get_states",
        description: "Get current state of Home Assistant entities. Provide entity_id for full details or domain to filter. Without filters, returns a compact summary (entity_id + state only).",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["light", "switch", "sensor", "temperature", "device", "thermostat", "climate", "door", "window", "lock", "plug", "energy"],
        voice_always: false,
    },
    ToolMeta {
        name: "ha_list_entities",
        description: "List Home Assistant entities, optionally filtered by domain.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["device", "entities", "what devices", "list devices"],
        voice_always: false,
    },
    ToolMeta {
        name: "config_version",
        description: "Get the current Yggdrasil configuration version.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["version"],
        voice_always: false,
    },
    ToolMeta {
        name: "web_search",
        description: "Search the web for current information. Returns titles, URLs, and descriptions of matching web pages.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["search", "look up", "lookup", "google", "web", "online", "latest", "news", "weather", "today", "current", "who is", "what is", "when is", "where is"],
        voice_always: false,
    },
    // ── Restricted tier (write operations) ────────────────────────
    ToolMeta {
        name: "ha_call_service",
        description: "Call a Home Assistant service to control devices. Allowed domains: light, switch, cover, fan, media_player, scene, script, input_boolean, input_number, input_select, input_text, automation, climate, vacuum, button, number, select, humidifier, water_heater.",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &["turn on", "turn off", "toggle", "light", "switch", "fan", "scene", "thermostat", "climate", "cover", "lock", "plug"],
        voice_always: false,
    },
    ToolMeta {
        name: "gaming",
        description: "Manage VMs and containers across Proxmox hosts (Thor, Plume). Actions: 'status' (all hosts/VMs/containers), 'launch' (wake host + assign GPU + start VM), 'stop' (shutdown VM or container + release GPU), 'start' (start VM or container), 'list-gpus' (GPU pool across hosts), 'pair' (Moonlight PIN for Sunshine). Supports gaming VMs (Harpy), inference VMs (Morrigan with llama-server), and service containers (Nightjar, Chirp, Gitea).",
        tier: ToolTier::Restricted,
        timeout_override_secs: Some(360),
        keywords: &["game", "gaming", "vm", "launch", "play", "moonlight", "harpy", "morrigan", "nightjar", "chirp", "plume", "thor", "inference", "code locally", "local llm"],
        voice_always: false,
    },
    ToolMeta {
        name: "store_memory",
        description: "Store a new engram as a cause/effect pair in memory.",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &["remember this", "save", "store", "note this"],
        voice_always: true,
    },
    ToolMeta {
        name: "context_offload",
        description: "Store, retrieve, or list large context blobs on the server. Use action 'store' to save content, 'retrieve' to fetch by handle, or 'list' to see all stored contexts.",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "context_bridge",
        description: "Export current session context as an engram for cross-session retrieval. Stores a snapshot of active work tagged 'context_bridge'. To import, use query_memory with the bridge label.",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "task_queue",
        description: "Manage the task queue (push, pop, complete, cancel, list).",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &["task", "todo", "queue", "remind me"],
        voice_always: false,
    },
    ToolMeta {
        name: "memory_graph",
        description: "Manage memory graph relationships (link, unlink, neighbors, traverse).",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    // ── MCP-only tools (not in Odin agent registry yet) ──────────
    ToolMeta {
        name: "generate",
        description: "Generate text using the local LLM. Intentionally excluded from Odin's agent registry to prevent recursive LLM calls.",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "delegate",
        description: "Delegate a task to the local LLM with full context assembly. Supports agent types: executor, docs, qa, review, general.",
        tier: ToolTier::Restricted,
        timeout_override_secs: Some(120),
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "task_delegate",
        description: "Delegate a task with structured output (legacy wrapper for delegate).",
        tier: ToolTier::Restricted,
        timeout_override_secs: Some(120),
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "build_check",
        description: "Run cargo build/check/clippy/test on the workspace. Requires cargo on the host.",
        tier: ToolTier::Restricted,
        timeout_override_secs: Some(120),
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "diff_review",
        description: "Review a git diff using the local LLM for code quality analysis.",
        tier: ToolTier::Restricted,
        timeout_override_secs: Some(60),
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "deploy",
        description: "Build and deploy Yggdrasil binaries to target nodes.",
        tier: ToolTier::Restricted,
        timeout_override_secs: Some(300),
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "vault",
        description: "Manage encrypted secrets (store, retrieve, list, delete).",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "config_sync",
        description: "Synchronize configuration files across workstations.",
        tier: ToolTier::Restricted,
        timeout_override_secs: None,
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "ha_generate_automation",
        description: "Generate a Home Assistant automation YAML from a natural language description.",
        tier: ToolTier::Restricted,
        timeout_override_secs: Some(60),
        keywords: &[],
        voice_always: false,
    },
    ToolMeta {
        name: "network_topology",
        description: "Query the mesh network topology — nodes, services, and health.",
        tier: ToolTier::Safe,
        timeout_override_secs: None,
        keywords: &["network", "topology", "nodes", "mesh"],
        voice_always: false,
    },
];

/// Look up a tool's metadata by name.
pub fn find_meta(name: &str) -> Option<&'static ToolMeta> {
    ALL_TOOLS.iter().find(|m| m.name == name)
}

/// Count tools by tier.
pub fn count_by_tier(tier: ToolTier) -> usize {
    ALL_TOOLS.iter().filter(|m| m.tier == tier).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_no_duplicate_names() {
        let mut names: Vec<&str> = ALL_TOOLS.iter().map(|m| m.name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate tool names in catalog");
    }

    #[test]
    fn all_tools_have_descriptions() {
        for meta in ALL_TOOLS {
            assert!(!meta.description.is_empty(), "tool '{}' has empty description", meta.name);
        }
    }

    #[test]
    fn find_meta_works() {
        assert!(find_meta("search_code").is_some());
        assert!(find_meta("nonexistent").is_none());
    }

    #[test]
    fn all_tools_have_schemas() {
        use crate::tool_params::schema_for_tool;
        for meta in ALL_TOOLS {
            assert!(
                schema_for_tool(meta.name).is_some(),
                "tool '{}' is missing a parameter schema in schema_for_tool()",
                meta.name
            );
        }
    }
}
