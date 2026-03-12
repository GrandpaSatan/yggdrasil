use tracing::debug;
use ygg_domain::mesh::{GateConfig, GatePolicy};

/// Evaluates gate policy rules to determine if a request should be allowed.
pub struct Gate {
    config: GateConfig,
}

impl Gate {
    pub fn new(config: GateConfig) -> Self {
        Self { config }
    }

    /// Check if a request from `source_node` to invoke `tool_name` is permitted.
    /// Returns true if the request is allowed.
    pub fn check(&self, source_node: &str, tool_name: &str) -> bool {
        for rule in &self.config.rules {
            if glob_matches(&rule.source, source_node) && glob_matches(&rule.tool, tool_name) {
                let allowed = rule.policy == GatePolicy::Allow;
                debug!(
                    source = source_node,
                    tool = tool_name,
                    policy = ?rule.policy,
                    "gate rule matched"
                );
                return allowed;
            }
        }

        // No rule matched — apply default policy.
        let allowed = self.config.default_policy == GatePolicy::Allow;
        debug!(
            source = source_node,
            tool = tool_name,
            default = ?self.config.default_policy,
            "gate: no rule matched, using default policy"
        );
        allowed
    }
}

/// Simple glob matching supporting "*" as a wildcard.
fn glob_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    glob_match::glob_match(pattern, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ygg_domain::mesh::GateRule;

    fn gate_with_rules(rules: Vec<GateRule>) -> Gate {
        Gate::new(GateConfig {
            default_policy: GatePolicy::Allow,
            rules,
        })
    }

    #[test]
    fn default_allows_when_no_rules() {
        let gate = gate_with_rules(vec![]);
        assert!(gate.check("munin", "search_code_tool"));
    }

    #[test]
    fn deny_rule_blocks_matching_request() {
        let gate = gate_with_rules(vec![GateRule {
            source: "thor".to_string(),
            tool: "ha_*".to_string(),
            policy: GatePolicy::Deny,
        }]);

        assert!(!gate.check("thor", "ha_call_service_tool"));
        assert!(gate.check("munin", "ha_call_service_tool"));
        assert!(gate.check("thor", "search_code_tool"));
    }

    #[test]
    fn wildcard_source_matches_all() {
        let gate = gate_with_rules(vec![GateRule {
            source: "*".to_string(),
            tool: "dangerous_tool".to_string(),
            policy: GatePolicy::Deny,
        }]);

        assert!(!gate.check("any_node", "dangerous_tool"));
        assert!(gate.check("any_node", "safe_tool"));
    }

    #[test]
    fn first_matching_rule_wins() {
        let gate = gate_with_rules(vec![
            GateRule {
                source: "thor".to_string(),
                tool: "ha_*".to_string(),
                policy: GatePolicy::Allow,
            },
            GateRule {
                source: "*".to_string(),
                tool: "ha_*".to_string(),
                policy: GatePolicy::Deny,
            },
        ]);

        // Thor is explicitly allowed by first rule
        assert!(gate.check("thor", "ha_call_service_tool"));
        // Others denied by second rule
        assert!(!gate.check("hugin", "ha_call_service_tool"));
    }
}
