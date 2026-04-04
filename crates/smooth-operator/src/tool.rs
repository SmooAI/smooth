use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

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

/// Configuration for parallel tool execution.
#[derive(Debug, Clone)]
pub struct ParallelExecutionConfig {
    pub max_concurrency: usize,
    pub timeout_per_tool: Duration,
}

impl Default for ParallelExecutionConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 5,
            timeout_per_tool: Duration::from_secs(30),
        }
    }
}

/// Registry of available tools with pre/post hooks.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    hooks: Vec<Arc<dyn ToolHook>>,
    parallel_config: ParallelExecutionConfig,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            hooks: vec![],
            parallel_config: ParallelExecutionConfig::default(),
        }
    }

    pub fn with_parallel_config(mut self, config: ParallelExecutionConfig) -> Self {
        self.parallel_config = config;
        self
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

    /// Execute multiple tool calls in parallel, respecting `max_concurrency` and `timeout_per_tool`.
    ///
    /// Each tool gets its own pre-hooks -> execute -> post-hooks cycle.
    /// If a tool exceeds `timeout_per_tool`, a `ToolResult` with `is_error=true` is returned.
    /// One failure does not cancel others. Results are returned in the same order as input calls.
    ///
    /// # Errors
    /// Individual tool errors are captured in the returned `ToolResult` (with `is_error=true`).
    /// This method itself does not return `Err`.
    pub async fn execute_parallel(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        if calls.is_empty() {
            return vec![];
        }

        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.parallel_config.max_concurrency));
        let timeout = self.parallel_config.timeout_per_tool;
        let tools = &self.tools;
        let hooks = &self.hooks;

        let mut join_set = tokio::task::JoinSet::new();
        // We track the original index so we can reorder results.
        for (index, call) in calls.iter().enumerate() {
            let call = call.clone();
            let semaphore = Arc::clone(&semaphore);
            let tools = tools.clone();
            let hooks: Vec<Arc<dyn ToolHook>> = hooks.clone();

            join_set.spawn(async move {
                let Ok(_permit) = semaphore.acquire().await else {
                    return (
                        index,
                        ToolResult {
                            tool_call_id: call.id.clone(),
                            content: "error: concurrency semaphore closed".to_string(),
                            is_error: true,
                        },
                    );
                };

                let result = tokio::time::timeout(timeout, async {
                    // Run pre-hooks
                    for hook in &hooks {
                        if let Err(e) = hook.pre_call(&call).await {
                            return ToolResult {
                                tool_call_id: call.id.clone(),
                                content: format!("blocked by hook: {e}"),
                                is_error: true,
                            };
                        }
                    }

                    // Find and execute tool
                    let result = match tools.get(&call.name) {
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
                    for hook in &hooks {
                        if let Err(e) = hook.post_call(&call, &result).await {
                            tracing::warn!(error = %e, tool = %call.name, "post-hook failed");
                        }
                    }

                    result
                })
                .await;

                let result = result.unwrap_or_else(|_| ToolResult {
                    tool_call_id: call.id.clone(),
                    content: "error: tool execution timed out".to_string(),
                    is_error: true,
                });

                (index, result)
            });
        }

        let mut results: Vec<Option<ToolResult>> = calls.iter().map(|_| None).collect();
        while let Some(join_result) = join_set.join_next().await {
            if let Ok((index, tool_result)) = join_result {
                results[index] = Some(tool_result);
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.unwrap_or_else(|| ToolResult {
                    tool_call_id: calls[i].id.clone(),
                    content: "error: task failed unexpectedly".to_string(),
                    is_error: true,
                })
            })
            .collect()
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

    // --- Parallel execution tests ---

    struct SlowTool {
        name: String,
        delay: std::time::Duration,
    }

    #[async_trait]
    impl Tool for SlowTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.clone(),
                description: "Sleeps then echoes".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            }
        }

        async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
            tokio::time::sleep(self.delay).await;
            Ok(arguments["text"].as_str().unwrap_or("done").to_string())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn parallel_two_tools_concurrent() {
        let mut registry = ToolRegistry::new();
        registry.register(SlowTool {
            name: "slow_a".into(),
            delay: std::time::Duration::from_secs(2),
        });
        registry.register(SlowTool {
            name: "slow_b".into(),
            delay: std::time::Duration::from_secs(2),
        });

        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "slow_a".into(),
                arguments: serde_json::json!({"text": "a"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "slow_b".into(),
                arguments: serde_json::json!({"text": "b"}),
            },
        ];

        let start = tokio::time::Instant::now();
        let results = registry.execute_parallel(&calls).await;
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert!(!results[1].is_error);
        // Both run concurrently, so wall time should be ~2s, not ~4s
        assert!(elapsed < std::time::Duration::from_secs(3), "elapsed: {elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn parallel_max_concurrency_1_is_sequential() {
        let config = ParallelExecutionConfig {
            max_concurrency: 1,
            timeout_per_tool: std::time::Duration::from_secs(30),
        };
        let mut registry = ToolRegistry::new().with_parallel_config(config);
        registry.register(SlowTool {
            name: "slow_a".into(),
            delay: std::time::Duration::from_secs(2),
        });
        registry.register(SlowTool {
            name: "slow_b".into(),
            delay: std::time::Duration::from_secs(2),
        });

        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "slow_a".into(),
                arguments: serde_json::json!({"text": "a"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "slow_b".into(),
                arguments: serde_json::json!({"text": "b"}),
            },
        ];

        let start = tokio::time::Instant::now();
        let results = registry.execute_parallel(&calls).await;
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 2);
        // With concurrency=1, must be sequential: >= 4s
        assert!(elapsed >= std::time::Duration::from_secs(4), "elapsed: {elapsed:?}");
    }

    #[tokio::test]
    async fn parallel_one_failure_does_not_cancel_others() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        registry.register(FailTool);

        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "ok"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "fail".into(),
                arguments: serde_json::json!({}),
            },
        ];

        let results = registry.execute_parallel(&calls).await;

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert_eq!(results[0].content, "ok");
        assert!(results[1].is_error);
        assert!(results[1].content.contains("intentional failure"));
    }

    #[tokio::test(start_paused = true)]
    async fn parallel_timeout_produces_error() {
        let config = ParallelExecutionConfig {
            max_concurrency: 5,
            timeout_per_tool: std::time::Duration::from_millis(500),
        };
        let mut registry = ToolRegistry::new().with_parallel_config(config);
        registry.register(SlowTool {
            name: "very_slow".into(),
            delay: std::time::Duration::from_secs(60),
        });

        let calls = vec![ToolCall {
            id: "c1".into(),
            name: "very_slow".into(),
            arguments: serde_json::json!({}),
        }];

        let results = registry.execute_parallel(&calls).await;

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0].content.contains("timed out"), "content: {}", results[0].content);
    }

    #[tokio::test]
    async fn parallel_pre_hook_blocks_one_tool_not_others() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        // Register a tool named "blocked_tool" so it exists
        registry.tools.insert("blocked_tool".into(), Arc::new(EchoTool) as Arc<dyn Tool>);
        registry.add_hook(BlockHook);

        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"text": "ok"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "blocked_tool".into(),
                arguments: serde_json::json!({"text": "nope"}),
            },
        ];

        let results = registry.execute_parallel(&calls).await;

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert_eq!(results[0].content, "ok");
        assert!(results[1].is_error);
        assert!(results[1].content.contains("blocked by hook"));
    }

    #[tokio::test]
    async fn parallel_results_in_same_order_as_input() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);

        let calls: Vec<ToolCall> = (0..10)
            .map(|i| ToolCall {
                id: format!("c{i}"),
                name: "echo".into(),
                arguments: serde_json::json!({"text": format!("msg-{i}")}),
            })
            .collect();

        let results = registry.execute_parallel(&calls).await;

        assert_eq!(results.len(), 10);
        for (i, result) in results.iter().enumerate() {
            assert_eq!(result.tool_call_id, format!("c{i}"));
            assert_eq!(result.content, format!("msg-{i}"));
        }
    }

    #[tokio::test]
    async fn parallel_empty_calls_returns_empty() {
        let registry = ToolRegistry::new();
        let results = registry.execute_parallel(&[]).await;
        assert!(results.is_empty());
    }
}
