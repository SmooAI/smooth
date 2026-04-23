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
    /// Optional structured details for UI rendering (diffs, tables, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ToolResult {
    /// Create a `ToolResult` with structured details attached.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Structured tool output that separates LLM-facing content from UI-facing details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Text content for the LLM to reason about.
    pub content: String,
    /// Structured data for UI rendering (diffs, tables, etc.).
    pub details: serde_json::Value,
}

/// JSON Schema definition for a tool parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Hook that runs before or after a tool call, with extended lifecycle
/// for network, shell, and filesystem operations.
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
    /// Called when a tool is about to make a network request. Return `Err` to block.
    async fn pre_network(&self, _url: &str, _method: &str) -> anyhow::Result<()> {
        Ok(())
    }
    /// Called when a tool is about to execute a shell command. Return `Err` to block.
    async fn pre_shell(&self, _command: &str) -> anyhow::Result<()> {
        Ok(())
    }
    /// Called when a tool is about to write to the filesystem. Return `Err` to block.
    async fn pre_write(&self, _path: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A tool that can be called by the agent.
#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String>;

    /// Whether this tool is safe to run concurrently with other tools.
    /// Defaults to `true`.
    fn is_concurrent_safe(&self) -> bool {
        true
    }

    /// Whether this tool only reads and has no side effects.
    /// Defaults to `false`.
    fn is_read_only(&self) -> bool {
        false
    }
}

#[async_trait]
impl Tool for Box<dyn Tool> {
    fn schema(&self) -> ToolSchema {
        (**self).schema()
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        (**self).execute(arguments).await
    }

    fn is_concurrent_safe(&self) -> bool {
        (**self).is_concurrent_safe()
    }

    fn is_read_only(&self) -> bool {
        (**self).is_read_only()
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
///
/// `Clone` is cheap — every inner value is `Arc`'d, so cloning the
/// registry gives a new handle that shares the same tool instances
/// and hook chain. The coding workflow relies on this to pass the
/// same tools into each phase's fresh `Agent`.
#[derive(Clone)]
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

    /// Register a pre-wrapped tool. Useful when the tool already needs to
    /// be `Arc<T>` for internal reasons (e.g. shared MCP service handles)
    /// so the caller can hand the same `Arc` to multiple registries.
    pub fn register_arc(&mut self, tool: Arc<dyn Tool>) {
        let schema = tool.schema();
        self.tools.insert(schema.name, tool);
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

    /// Look up a registered tool by name and clone the underlying
    /// `Arc<dyn Tool>`. Returns `None` if the tool isn't registered.
    ///
    /// Used by callers that need to forward a specific tool handle
    /// to another registry (e.g. the subagent dispatcher filtering
    /// the parent's tool set into a smaller per-subagent registry)
    /// without re-registering the underlying implementation.
    #[must_use]
    pub fn tool_by_name(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Check all hooks for a pending network request. Any `Err` blocks the operation.
    ///
    /// # Errors
    /// Returns error if any hook rejects the network request.
    pub async fn check_network(&self, url: &str, method: &str) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.pre_network(url, method).await?;
        }
        Ok(())
    }

    /// Check all hooks for a pending shell command. Any `Err` blocks the operation.
    ///
    /// # Errors
    /// Returns error if any hook rejects the shell command.
    pub async fn check_shell(&self, command: &str) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.pre_shell(command).await?;
        }
        Ok(())
    }

    /// Check all hooks for a pending filesystem write. Any `Err` blocks the operation.
    ///
    /// # Errors
    /// Returns error if any hook rejects the write operation.
    pub async fn check_write(&self, path: &str) -> anyhow::Result<()> {
        for hook in &self.hooks {
            hook.pre_write(path).await?;
        }
        Ok(())
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
                    details: None,
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
                    details: None,
                },
                Err(e) => ToolResult {
                    tool_call_id: call.id.clone(),
                    content: format!("error: {e}"),
                    is_error: true,
                    details: None,
                },
            },
            None => ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("unknown tool: {}", call.name),
                is_error: true,
                details: None,
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

    /// Execute a single tool call with hooks, used internally.
    async fn execute_single(tools: &HashMap<String, Arc<dyn Tool>>, hooks: &[Arc<dyn ToolHook>], call: &ToolCall) -> ToolResult {
        // Run pre-hooks
        for hook in hooks {
            if let Err(e) = hook.pre_call(call).await {
                return ToolResult {
                    tool_call_id: call.id.clone(),
                    content: format!("blocked by hook: {e}"),
                    is_error: true,
                    details: None,
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
                    details: None,
                },
                Err(e) => ToolResult {
                    tool_call_id: call.id.clone(),
                    content: format!("error: {e}"),
                    is_error: true,
                    details: None,
                },
            },
            None => ToolResult {
                tool_call_id: call.id.clone(),
                content: format!("unknown tool: {}", call.name),
                is_error: true,
                details: None,
            },
        };

        // Run post-hooks (don't block on failure)
        for hook in hooks {
            if let Err(e) = hook.post_call(call, &result).await {
                tracing::warn!(error = %e, tool = %call.name, "post-hook failed");
            }
        }

        result
    }

    /// Execute multiple tool calls with smart batching.
    ///
    /// Partitions calls into two batches:
    /// - Batch 1: concurrent-safe AND read-only tools run in parallel
    /// - Batch 2: all other tools run sequentially
    ///
    /// This is the Claude Code pattern for optimal latency: read-only tools
    /// can safely overlap, while write tools execute one at a time.
    ///
    /// Results are returned in the same order as input calls.
    ///
    /// # Errors
    /// Individual tool errors are captured in the returned `ToolResult` (with `is_error=true`).
    /// This method itself does not return `Err`.
    pub async fn execute_parallel(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        if calls.is_empty() {
            return vec![];
        }

        let mut results: Vec<Option<ToolResult>> = calls.iter().map(|_| None).collect();

        // Partition calls into parallel-safe (concurrent + read-only) and sequential
        let mut parallel_indices = Vec::new();
        let mut sequential_indices = Vec::new();

        for (i, call) in calls.iter().enumerate() {
            let (concurrent_safe, read_only) = self
                .tools
                .get(&call.name)
                .map_or((true, true), |tool| (tool.is_concurrent_safe(), tool.is_read_only()));

            if concurrent_safe && read_only {
                parallel_indices.push(i);
            } else {
                sequential_indices.push(i);
            }
        }

        // Batch 1: run parallel-safe tools concurrently
        if !parallel_indices.is_empty() {
            let semaphore = Arc::new(tokio::sync::Semaphore::new(self.parallel_config.max_concurrency));
            let timeout = self.parallel_config.timeout_per_tool;
            let tools = &self.tools;
            let hooks = &self.hooks;

            let mut join_set = tokio::task::JoinSet::new();
            for &index in &parallel_indices {
                let call = calls[index].clone();
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
                                details: None,
                            },
                        );
                    };

                    let result = tokio::time::timeout(timeout, Self::execute_single(&tools, &hooks, &call)).await;

                    let result = result.unwrap_or_else(|_| ToolResult {
                        tool_call_id: call.id.clone(),
                        content: "error: tool execution timed out".to_string(),
                        is_error: true,
                        details: None,
                    });

                    (index, result)
                });
            }

            while let Some(join_result) = join_set.join_next().await {
                if let Ok((index, tool_result)) = join_result {
                    results[index] = Some(tool_result);
                }
            }
        }

        // Batch 2: run sequential tools one at a time
        for &index in &sequential_indices {
            let call = &calls[index];
            let timeout = self.parallel_config.timeout_per_tool;

            let result = tokio::time::timeout(timeout, Self::execute_single(&self.tools, &self.hooks, call)).await;

            let result = result.unwrap_or_else(|_| ToolResult {
                tool_call_id: call.id.clone(),
                content: "error: tool execution timed out".to_string(),
                is_error: true,
                details: None,
            });

            results[index] = Some(result);
        }

        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.unwrap_or_else(|| ToolResult {
                    tool_call_id: calls[i].id.clone(),
                    content: "error: task failed unexpectedly".to_string(),
                    is_error: true,
                    details: None,
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

impl ToolRegistry {
    /// Clone the registry's tools (as `Arc` references) into a new registry.
    /// Hooks are NOT carried over — the new registry starts with no hooks.
    #[must_use]
    pub fn clone_tools(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            hooks: vec![],
            parallel_config: self.parallel_config.clone(),
        }
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
            details: None,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"is_error\":false"));
        // details should be omitted when None
        assert!(!json.contains("details"));
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

        fn is_read_only(&self) -> bool {
            true
        }

        fn is_concurrent_safe(&self) -> bool {
            true
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
        // FailTool is not read-only so it would go sequential.
        // Use a read-only fail tool for parallel testing.
        struct ReadOnlyFailTool;

        #[async_trait]
        impl Tool for ReadOnlyFailTool {
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

            fn is_read_only(&self) -> bool {
                true
            }
        }

        // Also make EchoTool read-only for this test
        struct ReadOnlyEchoTool;

        #[async_trait]
        impl Tool for ReadOnlyEchoTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "echo".into(),
                    description: "Echoes input back".into(),
                    parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                }
            }

            async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
                Ok(arguments["text"].as_str().unwrap_or("").to_string())
            }

            fn is_read_only(&self) -> bool {
                true
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(ReadOnlyEchoTool);
        registry.register(ReadOnlyFailTool);

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
        struct ReadOnlyEcho;

        #[async_trait]
        impl Tool for ReadOnlyEcho {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "echo".into(),
                    description: "Echoes".into(),
                    parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                }
            }

            async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
                Ok(arguments["text"].as_str().unwrap_or("").to_string())
            }

            fn is_read_only(&self) -> bool {
                true
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(ReadOnlyEcho);
        // Register a tool named "blocked_tool" so it exists
        struct BlockedReadOnly;

        #[async_trait]
        impl Tool for BlockedReadOnly {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "blocked_tool".into(),
                    description: "Will be blocked".into(),
                    parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                }
            }

            async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
                Ok(arguments["text"].as_str().unwrap_or("").to_string())
            }

            fn is_read_only(&self) -> bool {
                true
            }
        }

        registry.register(BlockedReadOnly);
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
        struct ReadOnlyEcho;

        #[async_trait]
        impl Tool for ReadOnlyEcho {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "echo".into(),
                    description: "Echoes".into(),
                    parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                }
            }

            async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
                Ok(arguments["text"].as_str().unwrap_or("").to_string())
            }

            fn is_read_only(&self) -> bool {
                true
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(ReadOnlyEcho);

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

    // -----------------------------------------------------------------------
    // New tests for extended hook lifecycle, ToolOutput, safety traits, and
    // smart batching
    // -----------------------------------------------------------------------

    /// pre_network hook blocks on Err
    #[tokio::test]
    async fn pre_network_hook_blocks_on_err() {
        struct BlockNetwork;

        #[async_trait]
        impl ToolHook for BlockNetwork {
            async fn pre_network(&self, url: &str, _method: &str) -> anyhow::Result<()> {
                if url.contains("evil.com") {
                    anyhow::bail!("network to evil.com is blocked");
                }
                Ok(())
            }
        }

        let mut registry = ToolRegistry::new();
        registry.add_hook(BlockNetwork);

        let err = registry.check_network("https://evil.com/api", "GET").await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("evil.com"));

        let ok = registry.check_network("https://good.com/api", "GET").await;
        assert!(ok.is_ok());
    }

    /// pre_shell hook blocks on Err
    #[tokio::test]
    async fn pre_shell_hook_blocks_on_err() {
        struct BlockShell;

        #[async_trait]
        impl ToolHook for BlockShell {
            async fn pre_shell(&self, command: &str) -> anyhow::Result<()> {
                if command.contains("rm -rf") {
                    anyhow::bail!("dangerous command blocked");
                }
                Ok(())
            }
        }

        let mut registry = ToolRegistry::new();
        registry.add_hook(BlockShell);

        let err = registry.check_shell("rm -rf /").await;
        assert!(err.is_err());

        let ok = registry.check_shell("ls -la").await;
        assert!(ok.is_ok());
    }

    /// pre_write hook blocks on Err
    #[tokio::test]
    async fn pre_write_hook_blocks_on_err() {
        struct BlockWrite;

        #[async_trait]
        impl ToolHook for BlockWrite {
            async fn pre_write(&self, path: &str) -> anyhow::Result<()> {
                if path.starts_with("/etc/") {
                    anyhow::bail!("writes to /etc/ are blocked");
                }
                Ok(())
            }
        }

        let mut registry = ToolRegistry::new();
        registry.add_hook(BlockWrite);

        let err = registry.check_write("/etc/passwd").await;
        assert!(err.is_err());

        let ok = registry.check_write("/tmp/safe.txt").await;
        assert!(ok.is_ok());
    }

    /// check_network iterates all hooks (second hook can block even if first allows)
    #[tokio::test]
    async fn check_network_iterates_all_hooks() {
        struct AllowAll;

        #[async_trait]
        impl ToolHook for AllowAll {}

        struct BlockEvil;

        #[async_trait]
        impl ToolHook for BlockEvil {
            async fn pre_network(&self, url: &str, _method: &str) -> anyhow::Result<()> {
                if url.contains("evil") {
                    anyhow::bail!("blocked by second hook");
                }
                Ok(())
            }
        }

        let mut registry = ToolRegistry::new();
        registry.add_hook(AllowAll);
        registry.add_hook(BlockEvil);

        let err = registry.check_network("https://evil.com", "GET").await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("second hook"));

        // Non-evil URL passes both hooks
        let ok = registry.check_network("https://good.com", "GET").await;
        assert!(ok.is_ok());
    }

    /// ToolOutput with details serialization
    #[test]
    fn tool_output_with_details_serialization() {
        let output = ToolOutput {
            content: "File changed".into(),
            details: serde_json::json!({
                "diff": "- old line\n+ new line",
                "path": "/src/main.rs"
            }),
        };
        let json = serde_json::to_string(&output).expect("serialize");
        let parsed: ToolOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.content, "File changed");
        assert_eq!(parsed.details["path"], "/src/main.rs");
    }

    /// ToolResult::with_details builder
    #[test]
    fn tool_result_with_details_builder() {
        let result = ToolResult {
            tool_call_id: "call-1".into(),
            content: "done".into(),
            is_error: false,
            details: None,
        }
        .with_details(serde_json::json!({"lines_changed": 42}));

        assert!(result.details.is_some());
        assert_eq!(result.details.as_ref().expect("details")["lines_changed"], 42);
        assert_eq!(result.content, "done");
        assert!(!result.is_error);

        // Verify it serializes correctly with details present
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"details\""));
        assert!(json.contains("42"));
    }

    /// is_concurrent_safe default is true
    #[test]
    fn is_concurrent_safe_default_is_true() {
        assert!(EchoTool.is_concurrent_safe());
    }

    /// is_read_only default is false
    #[test]
    fn is_read_only_default_is_false() {
        assert!(!EchoTool.is_read_only());
    }

    /// execute_parallel partitions by read_only (read tools parallel, write tools sequential)
    #[tokio::test(start_paused = true)]
    async fn execute_parallel_partitions_by_read_only() {
        struct ReadTool {
            name: String,
            delay: Duration,
        }

        #[async_trait]
        impl Tool for ReadTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: self.name.clone(),
                    description: "Read-only tool".into(),
                    parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                }
            }

            async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
                tokio::time::sleep(self.delay).await;
                Ok(arguments["text"].as_str().unwrap_or("read").to_string())
            }

            fn is_read_only(&self) -> bool {
                true
            }

            fn is_concurrent_safe(&self) -> bool {
                true
            }
        }

        struct WriteTool {
            name: String,
            delay: Duration,
        }

        #[async_trait]
        impl Tool for WriteTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: self.name.clone(),
                    description: "Write tool".into(),
                    parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                }
            }

            async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
                tokio::time::sleep(self.delay).await;
                Ok(arguments["text"].as_str().unwrap_or("write").to_string())
            }

            fn is_read_only(&self) -> bool {
                false
            }

            fn is_concurrent_safe(&self) -> bool {
                true
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(ReadTool {
            name: "read_a".into(),
            delay: Duration::from_secs(2),
        });
        registry.register(ReadTool {
            name: "read_b".into(),
            delay: Duration::from_secs(2),
        });
        registry.register(WriteTool {
            name: "write_a".into(),
            delay: Duration::from_secs(2),
        });
        registry.register(WriteTool {
            name: "write_b".into(),
            delay: Duration::from_secs(2),
        });

        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "read_a".into(),
                arguments: serde_json::json!({"text": "r1"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "read_b".into(),
                arguments: serde_json::json!({"text": "r2"}),
            },
            ToolCall {
                id: "c3".into(),
                name: "write_a".into(),
                arguments: serde_json::json!({"text": "w1"}),
            },
            ToolCall {
                id: "c4".into(),
                name: "write_b".into(),
                arguments: serde_json::json!({"text": "w2"}),
            },
        ];

        let start = tokio::time::Instant::now();
        let results = registry.execute_parallel(&calls).await;
        let elapsed = start.elapsed();

        // All four should succeed
        assert_eq!(results.len(), 4);
        for r in &results {
            assert!(!r.is_error, "unexpected error: {}", r.content);
        }

        // Read tools (2s each) run in parallel: ~2s
        // Write tools (2s each) run sequentially: ~4s
        // Total: ~6s (parallel reads overlap with nothing since they finish first,
        // then sequential writes run)
        // Should be >=6s and <7s
        assert!(
            elapsed >= Duration::from_secs(6),
            "expected >= 6s for 2 parallel reads + 2 sequential writes, got {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(7), "elapsed too long: {elapsed:?}");
    }

    /// Hook ordering preserved (first hook runs first, can block before second runs)
    #[tokio::test]
    async fn hook_ordering_preserved() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static CALL_ORDER: AtomicUsize = AtomicUsize::new(0);

        struct FirstHook;

        #[async_trait]
        impl ToolHook for FirstHook {
            async fn pre_network(&self, _url: &str, _method: &str) -> anyhow::Result<()> {
                let order = CALL_ORDER.fetch_add(1, Ordering::SeqCst);
                assert_eq!(order, 0, "FirstHook should run first");
                anyhow::bail!("blocked by first hook");
            }
        }

        struct SecondHook;

        #[async_trait]
        impl ToolHook for SecondHook {
            async fn pre_network(&self, _url: &str, _method: &str) -> anyhow::Result<()> {
                let _order = CALL_ORDER.fetch_add(1, Ordering::SeqCst);
                // Should never reach here because first hook blocks
                panic!("SecondHook should not run if first blocks");
            }
        }

        // Reset counter
        CALL_ORDER.store(0, Ordering::SeqCst);

        let mut registry = ToolRegistry::new();
        registry.add_hook(FirstHook);
        registry.add_hook(SecondHook);

        let err = registry.check_network("https://example.com", "GET").await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("first hook"));
        // Only first hook ran
        assert_eq!(CALL_ORDER.load(Ordering::SeqCst), 1);
    }
}
