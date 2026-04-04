//! 4-tier permission pipeline for tool call authorization.
//!
//! Every tool call passes through four tiers in order:
//!
//! 1. **Deny** — instantly blocked (dangerous patterns, blocked tools)
//! 2. **Allow** — instantly approved (read-only operations, safe tools)
//! 3. **Classify** — needs fast LLM judge (2 s timeout, future work)
//! 4. **Ask** — needs interactive user approval in the TUI

use async_trait::async_trait;
use serde_json::Value;
use smooth_operator::tool::ToolCall;

/// Permission decision tier for tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionTier {
    /// Instantly denied — dangerous patterns, blocked tools.
    Deny,
    /// Instantly allowed — read-only operations, safe tools.
    Allow,
    /// Needs classification by fast LLM judge (2 s timeout).
    Classify,
    /// Needs interactive user approval in TUI.
    Ask,
}

/// Result of a permission check.
#[derive(Debug, Clone)]
pub struct PermissionDecision {
    pub tier: PermissionTier,
    pub tool_name: String,
    pub reason: String,
    /// `true` if decided without user input (Deny or Allow tier).
    pub auto: bool,
}

// ---------------------------------------------------------------------------
// Dangerous argument patterns
// ---------------------------------------------------------------------------

/// Patterns that, if found anywhere in serialized tool arguments, trigger an
/// immediate Deny regardless of tool name.
const DANGEROUS_ARG_PATTERNS: &[&str] = &[
    "rm -rf",
    "sudo ",
    "chmod 777",
    "curl | sh",
    "curl |sh",
    "wget | sh",
    "wget |sh",
    "eval(",
    "exec(",
    "; rm ",
    "&& rm ",
];

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// The permission pipeline evaluates tool calls through 4 tiers.
pub struct PermissionPipeline {
    deny_patterns: Vec<String>,
    allow_patterns: Vec<String>,
}

impl Default for PermissionPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionPipeline {
    /// Create a pipeline pre-loaded with sensible defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            deny_patterns: vec![
                "rm -rf".into(),
                "sudo".into(),
                "chmod 777".into(),
                "curl | sh".into(),
                "eval".into(),
                "exec".into(),
            ],
            allow_patterns: vec![
                "read_file".into(),
                "code_search".into(),
                "find_definition".into(),
                "repo_map".into(),
                "grep".into(),
                "ls".into(),
                "cat".into(),
            ],
        }
    }

    /// Evaluate which tier a tool call falls into.
    #[must_use]
    pub fn evaluate(&self, tool_name: &str, arguments: &Value) -> PermissionDecision {
        // --- Tier 1: Deny ---------------------------------------------------
        // Check tool name against deny patterns.
        for pattern in &self.deny_patterns {
            if tool_name.contains(pattern.as_str()) {
                return PermissionDecision {
                    tier: PermissionTier::Deny,
                    tool_name: tool_name.to_string(),
                    reason: format!("tool name matches deny pattern: {pattern}"),
                    auto: true,
                };
            }
        }

        // Check serialized arguments for dangerous content.
        let args_str = arguments.to_string();
        for pattern in DANGEROUS_ARG_PATTERNS {
            if args_str.contains(pattern) {
                return PermissionDecision {
                    tier: PermissionTier::Deny,
                    tool_name: tool_name.to_string(),
                    reason: format!("arguments contain dangerous pattern: {pattern}"),
                    auto: true,
                };
            }
        }

        // --- Tier 2: Allow ---------------------------------------------------
        for pattern in &self.allow_patterns {
            if tool_name == pattern {
                return PermissionDecision {
                    tier: PermissionTier::Allow,
                    tool_name: tool_name.to_string(),
                    reason: format!("tool matches allow pattern: {pattern}"),
                    auto: true,
                };
            }
        }

        // --- Tier 3: Classify ------------------------------------------------
        // In the future this tier would invoke a fast LLM judge (e.g. Haiku or
        // Flash) with a 2 s timeout.  For now we simply return Classify and let
        // the caller decide.
        PermissionDecision {
            tier: PermissionTier::Classify,
            tool_name: tool_name.to_string(),
            reason: "tool not in deny or allow list — needs classification".into(),
            auto: false,
        }
    }

    /// Add a tool pattern to the deny list.
    pub fn deny(&mut self, pattern: &str) {
        self.deny_patterns.push(pattern.to_string());
    }

    /// Add a tool pattern to the allow list.
    pub fn allow(&mut self, pattern: &str) {
        self.allow_patterns.push(pattern.to_string());
    }
}

// ---------------------------------------------------------------------------
// ToolHook implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl smooth_operator::tool::ToolHook for PermissionPipeline {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        let decision = self.evaluate(&call.name, &call.arguments);
        match decision.tier {
            PermissionTier::Deny => {
                anyhow::bail!("Permission denied: {}", decision.reason);
            }
            PermissionTier::Allow => Ok(()),
            PermissionTier::Classify | PermissionTier::Ask => {
                // For now, allow with warning (TUI integration comes later).
                tracing::warn!(
                    tool = %call.name,
                    tier = ?decision.tier,
                    "permission check: would ask user"
                );
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline() -> PermissionPipeline {
        PermissionPipeline::new()
    }

    // 1. Deny tier for rm -rf pattern (in arguments)
    #[test]
    fn deny_tier_for_rm_rf_pattern() {
        let p = pipeline();
        let args = serde_json::json!({"command": "rm -rf /"});
        let d = p.evaluate("bash", &args);
        assert_eq!(d.tier, PermissionTier::Deny);
        assert!(d.auto);
        assert!(d.reason.contains("rm -rf"));
    }

    // 2. Deny tier for sudo pattern (in arguments)
    #[test]
    fn deny_tier_for_sudo_pattern() {
        let p = pipeline();
        let args = serde_json::json!({"command": "sudo apt install foo"});
        let d = p.evaluate("bash", &args);
        assert_eq!(d.tier, PermissionTier::Deny);
        assert!(d.auto);
        assert!(d.reason.contains("sudo"));
    }

    // 3. Allow tier for read_file
    #[test]
    fn allow_tier_for_read_file() {
        let p = pipeline();
        let d = p.evaluate("read_file", &serde_json::json!({"path": "src/main.rs"}));
        assert_eq!(d.tier, PermissionTier::Allow);
        assert!(d.auto);
    }

    // 4. Allow tier for code_search
    #[test]
    fn allow_tier_for_code_search() {
        let p = pipeline();
        let d = p.evaluate("code_search", &serde_json::json!({"query": "fn main"}));
        assert_eq!(d.tier, PermissionTier::Allow);
        assert!(d.auto);
    }

    // 5. Classify tier for unknown tool
    #[test]
    fn classify_tier_for_unknown_tool() {
        let p = pipeline();
        let d = p.evaluate("write_file", &serde_json::json!({"path": "foo.txt", "content": "hello"}));
        assert_eq!(d.tier, PermissionTier::Classify);
        assert!(!d.auto);
    }

    // 6. Custom deny pattern works
    #[test]
    fn custom_deny_pattern() {
        let mut p = pipeline();
        p.deny("dangerous_tool");
        let d = p.evaluate("dangerous_tool", &serde_json::json!({}));
        assert_eq!(d.tier, PermissionTier::Deny);
        assert!(d.auto);
    }

    // 7. Custom allow pattern works
    #[test]
    fn custom_allow_pattern() {
        let mut p = pipeline();
        p.allow("my_safe_tool");
        let d = p.evaluate("my_safe_tool", &serde_json::json!({}));
        assert_eq!(d.tier, PermissionTier::Allow);
        assert!(d.auto);
    }

    // 8. PermissionDecision has correct fields
    #[test]
    fn permission_decision_has_correct_fields() {
        let p = pipeline();
        let d = p.evaluate("grep", &serde_json::json!({"pattern": "TODO"}));
        assert_eq!(d.tier, PermissionTier::Allow);
        assert_eq!(d.tool_name, "grep");
        assert!(!d.reason.is_empty());
        assert!(d.auto);
    }
}
