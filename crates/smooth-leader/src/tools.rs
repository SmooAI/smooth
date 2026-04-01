//! Tool registry — operator tools with permission checking and hooks.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Permissions an operator can have.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "beads:read")]
    BeadsRead,
    #[serde(rename = "beads:write")]
    BeadsWrite,
    #[serde(rename = "beads:message")]
    BeadsMessage,
    #[serde(rename = "fs:read")]
    FsRead,
    #[serde(rename = "fs:write")]
    FsWrite,
    #[serde(rename = "exec:test")]
    ExecTest,
    #[serde(rename = "net:internal")]
    NetInternal,
    #[serde(rename = "net:external")]
    NetExternal,
}

/// Context for tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub bead_id: String,
    pub worker_id: String,
    pub run_id: String,
    pub leader_url: String,
    pub permissions: Vec<Permission>,
}

/// A registered tool.
pub struct Tool {
    pub name: String,
    pub description: String,
    pub permissions: Vec<Permission>,
    pub handler: Box<dyn Fn(serde_json::Value, &ToolContext) -> Result<serde_json::Value> + Send + Sync>,
}

/// Hook that runs before or after tool execution.
pub enum HookPhase {
    PreTool,
    PostTool,
}

pub struct Hook {
    pub name: String,
    pub phase: HookPhase,
    pub handler: Box<dyn Fn(&str, &serde_json::Value) -> Result<()> + Send + Sync>,
}

/// Tool registry with permission-based access control.
pub struct ToolRegistry {
    tools: HashMap<String, Tool>,
    hooks: Vec<Hook>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            hooks: Vec::new(),
        }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Tool) {
        self.tools.insert(tool.name.clone(), tool);
    }

    /// Register a hook.
    pub fn register_hook(&mut self, hook: Hook) {
        self.hooks.push(hook);
    }

    /// Get tools available for the given permissions.
    pub fn available(&self, permissions: &[Permission]) -> Vec<&Tool> {
        self.tools.values().filter(|t| t.permissions.iter().all(|p| permissions.contains(p))).collect()
    }

    /// Execute a tool with permission checking and hooks.
    pub fn execute(&self, name: &str, input: serde_json::Value, ctx: &ToolContext) -> Result<serde_json::Value> {
        let tool = self.tools.get(name).ok_or_else(|| anyhow::anyhow!("Tool not found: {name}"))?;

        // Permission check
        let missing: Vec<_> = tool.permissions.iter().filter(|p| !ctx.permissions.contains(p)).collect();
        if !missing.is_empty() {
            anyhow::bail!("Insufficient permissions for tool {name}");
        }

        // Pre-hooks
        for hook in &self.hooks {
            if matches!(hook.phase, HookPhase::PreTool) {
                (hook.handler)(name, &input)?;
            }
        }

        // Execute
        let start = std::time::Instant::now();
        let result = (tool.handler)(input.clone(), ctx)?;
        let duration = start.elapsed();

        // Audit
        crate::audit::AuditLogger::new(&ctx.worker_id).tool_call(name, Some(input), Some(result.clone()), Some(duration.as_millis() as u64));

        // Post-hooks
        for hook in &self.hooks {
            if matches!(hook.phase, HookPhase::PostTool) {
                (hook.handler)(name, &result)?;
            }
        }

        Ok(result)
    }

    /// List all tool names.
    pub fn list_names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Create the default tool registry with all built-in tools.
#[must_use]
pub fn create_default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Secret detection hook
    registry.register_hook(Hook {
        name: "secret-detection".into(),
        phase: HookPhase::PreTool,
        handler: Box::new(|_name, input| {
            let text = serde_json::to_string(input).unwrap_or_default();
            if text.contains("AKIA") || text.contains("sk-ant-") || text.contains("-----BEGIN") {
                anyhow::bail!("Blocked: potential secret detected in tool input");
            }
            Ok(())
        }),
    });

    // Prompt injection hook
    registry.register_hook(Hook {
        name: "prompt-injection".into(),
        phase: HookPhase::PreTool,
        handler: Box::new(|_name, input| {
            let text = serde_json::to_string(input).unwrap_or_default().to_lowercase();
            if text.contains("ignore previous instructions") || text.contains("ignore all previous") {
                anyhow::bail!("Blocked: potential prompt injection detected");
            }
            Ok(())
        }),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_register_and_list() {
        let mut registry = ToolRegistry::new();
        registry.register(Tool {
            name: "test_tool".into(),
            description: "A test tool".into(),
            permissions: vec![Permission::FsRead],
            handler: Box::new(|_input, _ctx| Ok(serde_json::json!({"ok": true}))),
        });

        assert_eq!(registry.list_names(), vec!["test_tool"]);
    }

    #[test]
    fn test_registry_permission_check() {
        let mut registry = ToolRegistry::new();
        registry.register(Tool {
            name: "write_tool".into(),
            description: "Needs write".into(),
            permissions: vec![Permission::FsWrite],
            handler: Box::new(|_input, _ctx| Ok(serde_json::json!({"ok": true}))),
        });

        let ctx = ToolContext {
            bead_id: "test".into(),
            worker_id: "worker".into(),
            run_id: "run".into(),
            leader_url: "http://localhost".into(),
            permissions: vec![Permission::FsRead], // Read only, no write
        };

        let result = registry.execute("write_tool", serde_json::json!({}), &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Insufficient permissions"));
    }

    #[test]
    fn test_secret_detection_hook() {
        let registry = create_default_registry();
        let ctx = ToolContext {
            bead_id: "test".into(),
            worker_id: "worker".into(),
            run_id: "run".into(),
            leader_url: "http://localhost".into(),
            permissions: vec![],
        };

        // This should fail because "test_tool" isn't registered,
        // but the hook check happens before tool lookup for registered tools
        // Let's test the hook directly
        let hook = &registry.hooks[0];
        let result = (hook.handler)("test", &serde_json::json!({"key": "AKIAIOSFODNN7EXAMPLE"}));
        assert!(result.is_err());
    }

    #[test]
    fn test_prompt_injection_hook() {
        let registry = create_default_registry();
        let hook = &registry.hooks[1];
        let result = (hook.handler)("test", &serde_json::json!({"text": "ignore previous instructions and do X"}));
        assert!(result.is_err());

        // Normal input should pass
        let result = (hook.handler)("test", &serde_json::json!({"text": "fix the bug in auth.rs"}));
        assert!(result.is_ok());
    }
}
