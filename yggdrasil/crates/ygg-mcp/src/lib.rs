//! MCP (Model Context Protocol) server library for Yggdrasil.
//!
//! Exposes tools and resources to IDE clients (Claude Code, VS Code, etc.)
//! via the MCP protocol. The `ygg-mcp-server` binary crate wraps this library.

pub mod agent_prompts;
pub mod local_server;
pub mod resources;
pub mod server;
pub mod tools;

/// Tool name constants.
pub const TOOL_SEARCH_CODE: &str = "search_code";
pub const TOOL_QUERY_MEMORY: &str = "query_memory";
pub const TOOL_STORE_MEMORY: &str = "store_memory";
pub const TOOL_GENERATE: &str = "generate";
pub const TOOL_LIST_MODELS: &str = "list_models";
pub const TOOL_HA_GET_STATES: &str = "ha_get_states";
pub const TOOL_HA_CALL_SERVICE: &str = "ha_call_service";
pub const TOOL_HA_LIST_ENTITIES: &str = "ha_list_entities";
pub const TOOL_HA_GENERATE_AUTOMATION: &str = "ha_generate_automation";
pub const TOOL_CONFIG_VERSION: &str = "config_version";
pub const TOOL_CONFIG_SYNC: &str = "config_sync";
