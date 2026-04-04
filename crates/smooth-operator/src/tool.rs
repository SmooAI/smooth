use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

/// JSON Schema definition for a tool parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Hook that runs before or after a tool call.
#[async_trait]
pub trait ToolHook: Send + Sync {
    /// Called before tool execution. Return `Err` to block the call.
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        let _ = call;
        Ok(())
    }
    /// Called after tool execution with the result.
    async fn post_call(&self, call: &ToolCall, result: &ToolResult) -> anyhow::Result<()> {
        let _ = (call, result);
        Ok(())
    }
}

/// A tool that can be called by the agent.
#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String>;
}

#[async_trait]
impl Tool for Box<dyn Tool> {
    fn schema(&self) -> ToolSchema {
        (**self).schema()
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        (**self).execute(arguments).await
    }
}

/// Registry of available tools with pre/post hooks.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    hooks: Vec<Arc<dyn ToolHook>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            hooks: vec![],
        }
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        let schema = tool.schema();
        self.tools.insert(schema.name, Arc::new(tool));
    }

    pub fn add_hook(&mut self, hook: impl ToolHook + 'static) {
        self.hooks.push(Arc::new(hook));
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Execute a tool call, running all hooks.
    ///
    /// # Errors
    /// Returns error if a pre-hook blocks the call, the tool is not found,
    /// or the tool execution fails.
    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        // Run pre-hooks
        for hook in &self.hooks {
            if let Err(e) = hook.pre_call(call).await {
                return ToolResult {
                    tool_call_id: call.id.clone(),
                    content: format!("blocked by hook: {e}"),
                    is_error: true,
                };
            }
        }

        // Find and execute tool
        let result = match self.tools.get(&call.name) {
            Some(tool) => match tool.execute(call.arguments.clone()).await {
                Ok(content) => ToolResult {
                    tool_call_id: call.id.clone(),
                    content,
                    is_error: false,
                },
                Err(e) => ToolResult {
                    tool_call_id: call.id.clone(),
                    content: format!("error: {e}"),
                    is_error: true,
                },
            },
            None => ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("unknown tool: {}", call.name),
                is_error: true,
            },
        };

        // Run post-hooks (don't block on failure)
        for hook in &self.hooks {
            if let Err(e) = hook.post_call(call, &result).await {
                tracing::warn!(error = %e, tool = %call.name, "post-hook failed");
            }
        }

        result
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "echo".into(),
                description: "Echoes input back".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
            }
        }

        async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
            Ok(arguments["text"].as_str().unwrap_or("").to_string())
        }
    }

    struct FailTool;

    #[async_trait]
    impl Tool for FailTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "fail".into(),
                description: "Always fails".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(&self, _arguments: serde_json::Value) -> anyhow::Result<String> {
            anyhow::bail!("intentional failure")
        }
    }

    struct BlockHook;

    #[async_trait]
    impl ToolHook for BlockHook {
        async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
            if call.name == "blocked_tool" {
                anyhow::bail!("tool is blocked by policy");
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_echo_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);

        let call = ToolCall {
            id: "call-1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({"text": "hello world"}),
        };

        let result = registry.execute(&call).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "hello world");
    }

    #[tokio::test]
    async fn execute_unknown_tool() {
        let registry = ToolRegistry::new();
        let call = ToolCall {
            id: "call-1".into(),
            name: "nonexistent".into(),
            arguments: serde_json::json!({}),
        };

        let result = registry.execute(&call).await;
        assert!(result.is_error);
        assert!(result.content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn execute_failing_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(FailTool);

        let call = ToolCall {
            id: "call-1".into(),
            name: "fail".into(),
            arguments: serde_json::json!({}),
        };

        let result = registry.execute(&call).await;
        assert!(result.is_error);
        assert!(result.content.contains("intentional failure"));
    }

    #[tokio::test]
    async fn hook_blocks_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        registry.add_hook(BlockHook);

        let call = ToolCall {
            id: "call-1".into(),
            name: "blocked_tool".into(),
            arguments: serde_json::json!({}),
        };

        let result = registry.execute(&call).await;
        assert!(result.is_error);
        assert!(result.content.contains("blocked by hook"));
    }

    #[tokio::test]
    async fn hook_allows_other_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        registry.add_hook(BlockHook);

        let call = ToolCall {
            id: "call-1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({"text": "allowed"}),
        };

        let result = registry.execute(&call).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "allowed");
    }

    #[test]
    fn registry_schemas() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        registry.register(FailTool);

        let schemas = registry.schemas();
        assert_eq!(schemas.len(), 2);
        let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"fail"));
    }

    #[test]
    fn has_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        assert!(registry.has_tool("echo"));
        assert!(!registry.has_tool("missing"));
    }

    #[test]
    fn tool_call_serialization() {
        let call = ToolCall {
            id: "call-1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({"text": "hi"}),
        };
        let json = serde_json::to_string(&call).expect("serialize");
        let parsed: ToolCall = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.name, "echo");
    }

    #[test]
    fn tool_result_serialization() {
        let result = ToolResult {
            tool_call_id: "call-1".into(),
            content: "output".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"is_error\":false"));
    }
}
