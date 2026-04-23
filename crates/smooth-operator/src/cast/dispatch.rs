//! # `send_sidekick` tool
//!
//! The tool a lead role (`fixer` / `runner`) calls to hand a
//! self-contained task to one of the registered sidekicks
//! ([`RoleKind::Sidekick`]). The sidekick runs in its own [`Agent`]
//! loop with a fresh conversation, a filtered [`ToolRegistry`] scoped
//! to exactly the tools the sidekick is permitted to use, and its own
//! [`PermissionHook`]. The parent receives a single JSON tool result
//! — `{agent, turns, final_message}` — and nothing else. The
//! sidekick's transcript (its individual LLM calls, intermediate
//! reasoning, and tool calls) is never injected into the parent's
//! conversation.
//!
//! ## Why this tool is in `smooth-operator`
//!
//! The dispatch tool needs access to [`Agent`], [`ToolRegistry`],
//! [`LlmConfig`], and [`Cast`], which all live in
//! `smooth-operator`. Keeping the tool here — instead of in
//! `smooth-operator-runner` — means the runner just registers it
//! alongside any other tool when the active lead role is
//! dispatchable (`fixer` or `runner`), and other callers
//! (benchmarks, the coding workflow, host-side eval harnesses) can
//! reuse the exact same dispatch surface without pulling in the
//! runner.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentConfig, AgentEvent};
use crate::cast::{Cast, PermissionHook, RoleKind};
use crate::llm::LlmConfig;
use crate::providers::Activity;
use crate::tool::{Tool, ToolRegistry, ToolSchema};

/// Closure type the dispatch tool uses to resolve an [`Activity`]
/// slot into a concrete [`LlmConfig`].
///
/// The parent of the dispatch tool (typically the runner) owns the
/// [`ProviderRegistry`](crate::providers::ProviderRegistry) or equivalent
/// routing config and hands a small closure to the tool so the tool
/// doesn't need to know the routing shape. Keeping the factory as a
/// closure also makes the tool trivial to unit-test: tests provide a
/// closure that returns a config pointing at an in-process mock HTTP
/// server.
pub type LlmConfigFactory = Arc<dyn Fn(Activity) -> anyhow::Result<LlmConfig> + Send + Sync>;

/// Input schema for the `send_sidekick` tool, kept as a typed
/// struct so deserialization errors surface clearly in the tool
/// result instead of being silently-ignored missing fields.
#[derive(Debug, Deserialize)]
struct DispatchArgs {
    /// Name of the sidekick to dispatch (must be registered in the
    /// [`Cast`] with [`RoleKind::Sidekick`]).
    agent: String,
    /// The prompt / task description handed to the sidekick as its
    /// user message. The sidekick's system prompt comes from its
    /// [`OperatorRole`](crate::cast::OperatorRole); `prompt` is the
    /// per-call instruction.
    prompt: String,
}

/// JSON shape of a successful `send_sidekick` tool result.
///
/// Public so downstream callers (tests, TUI renderers) can
/// deserialize it without reparsing free text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchResult {
    /// The sidekick name that ran.
    pub agent: String,
    /// How many outer agent-loop iterations the sidekick used.
    /// Useful for budget accounting and for the parent to decide
    /// whether to redispatch with a larger cap.
    pub turns: u32,
    /// The final assistant message the sidekick produced. This is
    /// the only textual content that crosses the boundary back
    /// into the parent's conversation — everything else (tool
    /// calls, intermediate reasoning, retried turns) stays
    /// isolated in the sidekick's own conversation.
    pub final_message: String,
}

/// Built-in tool that hands a task to a sidekick and returns only its
/// final summary to the parent.
pub struct DispatchSubagentTool {
    cast: Arc<Cast>,
    /// Snapshot of the parent's [`ToolRegistry`] at construction
    /// time. The sidekick's registry is built by filtering this
    /// snapshot to the sidekick's allowed tool set.
    parent_tools: ToolRegistry,
    llm_factory: LlmConfigFactory,
    /// Max iterations for the spawned sidekick. Copied onto the
    /// fresh [`AgentConfig`] unless the sidekick's own
    /// [`OperatorRole::steps`](crate::cast::OperatorRole::steps)
    /// override is set.
    default_max_iterations: u32,
    /// Max context tokens for the spawned sidekick. Sidekicks run
    /// short, so we default smaller than the parent — but still
    /// generous enough for an investigation pass.
    default_max_context_tokens: usize,
}

impl DispatchSubagentTool {
    /// Build a new dispatch tool.
    ///
    /// - `cast` — registry to look up sidekick definitions by name.
    /// - `parent_tools` — a clone of the parent's tool registry; the
    ///   sidekick's registry is filtered down from this.
    /// - `llm_factory` — closure mapping [`Activity`] to
    ///   [`LlmConfig`]. The caller owns routing.
    #[must_use]
    pub fn new(cast: Arc<Cast>, parent_tools: ToolRegistry, llm_factory: LlmConfigFactory) -> Self {
        Self {
            cast,
            parent_tools,
            llm_factory,
            default_max_iterations: 20,
            default_max_context_tokens: 64_000,
        }
    }

    /// Override the default max iterations for spawned sidekicks.
    /// Mostly useful in tests where you want to cap tightly.
    #[must_use]
    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.default_max_iterations = max;
        self
    }

    /// Override the default max context tokens for spawned
    /// sidekicks.
    #[must_use]
    pub fn with_max_context_tokens(mut self, tokens: usize) -> Self {
        self.default_max_context_tokens = tokens;
        self
    }

    /// Build a filtered [`ToolRegistry`] that contains only the
    /// tools the sidekick is permitted to call, plus a
    /// [`PermissionHook`] that enforces the sidekick's
    /// [`Clearance`](crate::cast::Clearance) at dispatch
    /// time.
    ///
    /// The filter uses [`Clearance::allows`] so both allow-list
    /// and deny-list semantics match what the runner would apply.
    /// The hook is kept in the registry as a second line of defense
    /// — if a future code path bypasses the tool filter (e.g. by
    /// looking up a tool by name directly), the hook still blocks
    /// the call before it runs.
    fn build_subagent_tools(&self, role: &crate::cast::OperatorRole) -> ToolRegistry {
        let mut filtered = ToolRegistry::new();
        for schema in self.parent_tools.schemas() {
            if !role.permissions.allows(&schema.name) {
                continue;
            }
            // Skip recursive dispatch — a sidekick must not be able
            // to spawn further sidekicks via the same tool. If we
            // ever want that, we'll add it deliberately.
            if schema.name == Self::TOOL_NAME {
                continue;
            }
            if let Some(tool) = self.parent_tools.tool_by_name(&schema.name) {
                filtered.register_arc(tool);
            }
        }
        filtered.add_hook(PermissionHook::new(role));
        filtered
    }

    /// Name the tool registers under. Callers building a parent
    /// tool registry use this to detect "is dispatch available?".
    pub const TOOL_NAME: &'static str = "send_sidekick";
}

#[async_trait]
impl Tool for DispatchSubagentTool {
    fn schema(&self) -> ToolSchema {
        // Build the agent-name enum dynamically from the cast
        // so the schema always matches what's dispatchable. If
        // someone adds a new sidekick, the LLM sees it in the enum
        // without any prompt surgery.
        let sidekick_names: Vec<String> = self.cast.sidekicks().map(|a| a.name.clone()).collect();
        let enum_values: Vec<serde_json::Value> = sidekick_names.iter().map(|n| serde_json::Value::String(n.clone())).collect();

        ToolSchema {
            name: Self::TOOL_NAME.into(),
            description: "Dispatch a self-contained task to a named sidekick. \
                 The sidekick runs in its own isolated conversation with its \
                 own tools and permissions, and returns only a final summary \
                 — its transcript never enters yours. Use `scout` for \
                 read-only investigation (find + summarize) and `runner` \
                 for multi-step tasks that need full tool access."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "enum": enum_values,
                        "description": "Which sidekick to dispatch."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task description handed to the sidekick. Be specific — the sidekick has no other context from this conversation."
                    }
                },
                "required": ["agent", "prompt"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let args: DispatchArgs = serde_json::from_value(arguments).map_err(|e| anyhow::anyhow!("invalid send_sidekick arguments: {e}"))?;

        // Resolve the sidekick. Unknown names and non-sidekick kinds
        // (lead, shadow) both return the same "not a dispatchable
        // sidekick" error — we don't want the dispatch tool to
        // become a backdoor for spawning shadow utility roles or
        // lead roles.
        let sub = match self.cast.get(&args.agent) {
            Some(a) if a.kind == RoleKind::Sidekick => a.clone(),
            _ => return Err(anyhow::anyhow!("'{}' is not a dispatchable sidekick", args.agent)),
        };

        // Resolve the LLM config for the sidekick's routing slot.
        let llm = (self.llm_factory)(sub.slot).map_err(|e| anyhow::anyhow!("failed to resolve LLM config for sidekick '{}': {e}", sub.name))?;

        // Build a fresh, isolated conversation via a fresh Agent.
        let max_iterations = sub.steps.unwrap_or(self.default_max_iterations);
        let mut config = AgentConfig::new(format!("sub-{}", sub.name), &sub.prompt, llm).with_max_iterations(max_iterations);
        config.max_context_tokens = self.default_max_context_tokens;

        // Filtered tool surface scoped to the sidekick's permissions.
        let tools = self.build_subagent_tools(&sub);

        let agent = Agent::new(config, tools);

        // Sidekick events go to a LOCAL channel that is never wired
        // back to the parent's event stream. This is the core
        // isolation guarantee: whatever the sidekick emits
        // (LlmRequest, ToolCallStart, TokenDelta, …) is consumed
        // here and not forwarded. The parent only sees the single
        // tool result we return below.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let drain = tokio::spawn(async move {
            // Drop every event. We track the iteration count out of
            // the returned Conversation instead of reading events,
            // so this drain can stay dumb.
            while rx.recv().await.is_some() {}
        });

        let prompt = args.prompt.clone();
        let conversation = agent
            .run_with_channel(prompt, tx)
            .await
            .map_err(|e| anyhow::anyhow!("sidekick '{}' run failed: {e}", sub.name))?;

        // Wait for the drain task to finish (channel closed when
        // run_with_channel returned).
        let _ = drain.await;

        // Pull out the final assistant message. If the sidekick hit
        // its iteration cap without ever producing a final assistant
        // message, surface that explicitly so the parent doesn't
        // get a misleading empty summary.
        let final_message = conversation.last_assistant_content().ok_or_else(|| {
            anyhow::anyhow!(
                "sidekick '{}' produced no assistant message (likely hit the {max_iterations}-iteration cap without completing)",
                sub.name
            )
        })?;

        // Count outer-loop turns. `assistant` messages with content
        // or tool calls map 1:1 to agent-loop iterations — that's
        // the useful "turns" number for budget tooling. We count
        // assistant messages directly instead of plumbing the
        // iteration count out of run_with_channel.
        let turns = conversation.assistant_turn_count();

        let result = DispatchResult {
            agent: sub.name.clone(),
            turns,
            final_message: final_message.to_string(),
        };

        serde_json::to_string(&result).map_err(|e| anyhow::anyhow!("failed to serialize dispatch result: {e}"))
    }

    fn is_concurrent_safe(&self) -> bool {
        // Sidekicks can freely share a parent's tool Arcs and run in
        // parallel with other read-only operations from the parent's
        // perspective. The ToolRegistry's smart batching already
        // serializes writes via is_read_only; dispatch itself is
        // neither read-only nor safe to batch alongside a write, so
        // mark it non-read-only (the default) but concurrent-safe.
        true
    }

    fn is_read_only(&self) -> bool {
        // A `runner` sidekick dispatch can write files via its
        // inherited tools. Don't let the registry's read-only
        // parallel batch run it alongside another write.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cast::Cast;
    use crate::llm::ApiFormat;
    use serde_json::json;

    fn test_llm_factory() -> LlmConfigFactory {
        Arc::new(|_activity: Activity| -> anyhow::Result<LlmConfig> {
            // A config that will never actually be used because the
            // tests below bail before hitting the LLM path.
            Ok(LlmConfig {
                api_url: "http://127.0.0.1:1".into(),
                api_key: "test".into(),
                model: "test".into(),
                max_tokens: 8192,
                temperature: 0.0,
                retry_policy: crate::llm::RetryPolicy::default(),
                api_format: ApiFormat::OpenAiCompat,
            })
        })
    }

    #[test]
    fn schema_lists_registered_sidekicks_in_enum() {
        let cast = Arc::new(Cast::builtin());
        let tool = DispatchSubagentTool::new(Arc::clone(&cast), ToolRegistry::new(), test_llm_factory());
        let schema = tool.schema();
        assert_eq!(schema.name, "send_sidekick");
        let enum_values = &schema.parameters["properties"]["agent"]["enum"];
        let names: Vec<&str> = enum_values
            .as_array()
            .expect("enum array")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert!(names.contains(&"scout"), "schema enum missing scout: {names:?}");
        assert!(names.contains(&"runner"), "schema enum missing runner: {names:?}");
        // Lead/shadow roles must not appear.
        for bad in ["fixer", "mapper", "oracle", "heckler", "tagger", "presser", "recapper"] {
            assert!(!names.contains(&bad), "schema enum must not contain non-sidekick '{bad}': {names:?}");
        }
    }

    #[tokio::test]
    async fn unknown_agent_name_returns_error() {
        let cast = Arc::new(Cast::builtin());
        let tool = DispatchSubagentTool::new(Arc::clone(&cast), ToolRegistry::new(), test_llm_factory());
        let err = tool
            .execute(json!({"agent": "nonexistent", "prompt": "hello"}))
            .await
            .expect_err("unknown agent must error");
        let msg = err.to_string();
        assert!(msg.contains("not a dispatchable sidekick"), "unexpected error: {msg}");
        assert!(msg.contains("nonexistent"), "error should name the bad agent: {msg}");
    }

    #[tokio::test]
    async fn lead_role_name_returns_error() {
        // 'fixer' is a Lead, not a Sidekick — dispatching to it
        // must be blocked with the same "not a dispatchable
        // sidekick" error, NOT fall through to spawning a `fixer`
        // agent loop.
        let cast = Arc::new(Cast::builtin());
        let tool = DispatchSubagentTool::new(Arc::clone(&cast), ToolRegistry::new(), test_llm_factory());
        let err = tool
            .execute(json!({"agent": "fixer", "prompt": "do something"}))
            .await
            .expect_err("lead role dispatch must error");
        let msg = err.to_string();
        assert!(msg.contains("not a dispatchable sidekick"), "unexpected error: {msg}");
        assert!(msg.contains("fixer"), "error should name the bad agent: {msg}");
    }

    #[tokio::test]
    async fn shadow_role_name_returns_error() {
        // 'tagger' is a Shadow utility role — also not dispatchable.
        let cast = Arc::new(Cast::builtin());
        let tool = DispatchSubagentTool::new(Arc::clone(&cast), ToolRegistry::new(), test_llm_factory());
        let err = tool
            .execute(json!({"agent": "tagger", "prompt": "name this"}))
            .await
            .expect_err("shadow role dispatch must error");
        assert!(err.to_string().contains("not a dispatchable sidekick"));
    }

    #[tokio::test]
    async fn malformed_arguments_return_error() {
        let cast = Arc::new(Cast::builtin());
        let tool = DispatchSubagentTool::new(Arc::clone(&cast), ToolRegistry::new(), test_llm_factory());
        // Missing `prompt` field.
        let err = tool.execute(json!({"agent": "scout"})).await.expect_err("missing prompt must error");
        assert!(err.to_string().contains("invalid send_sidekick arguments"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn dispatch_result_serializes_to_expected_shape() {
        // Direct check of the result type's JSON shape — the
        // parent's tool call sees exactly this shape and nothing
        // else from the sidekick transcript.
        let result = DispatchResult {
            agent: "scout".into(),
            turns: 3,
            final_message: "found 4 usages of X in src/".into(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed["agent"], "scout");
        assert_eq!(parsed["turns"], 3);
        assert_eq!(parsed["final_message"], "found 4 usages of X in src/");
        // No stray fields that could leak transcript content.
        let obj = parsed.as_object().expect("object");
        assert_eq!(obj.len(), 3, "DispatchResult must have exactly 3 fields, got {obj:?}");
    }

    #[test]
    fn build_subagent_tools_filters_by_permissions() {
        use async_trait::async_trait;

        struct DummyTool(&'static str);
        #[async_trait]
        impl Tool for DummyTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: self.0.into(),
                    description: "dummy".into(),
                    parameters: json!({"type": "object"}),
                }
            }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<String> {
                Ok("ok".into())
            }
        }

        let mut parent_tools = ToolRegistry::new();
        parent_tools.register(DummyTool("read_file"));
        parent_tools.register(DummyTool("edit_file"));
        parent_tools.register(DummyTool("bash"));
        parent_tools.register(DummyTool("grep"));
        parent_tools.register(DummyTool("send_sidekick")); // should be filtered out — no recursive dispatch

        let cast = Arc::new(Cast::builtin());
        let tool = DispatchSubagentTool::new(Arc::clone(&cast), parent_tools, test_llm_factory());
        let scout = cast.get("scout").unwrap();

        let filtered = tool.build_subagent_tools(scout);
        let names: Vec<String> = filtered.schemas().into_iter().map(|s| s.name).collect();

        assert!(names.contains(&"read_file".to_string()), "read_file missing: {names:?}");
        assert!(names.contains(&"grep".to_string()), "grep missing: {names:?}");
        assert!(!names.contains(&"edit_file".to_string()), "edit_file leaked: {names:?}");
        assert!(!names.contains(&"bash".to_string()), "bash leaked: {names:?}");
        assert!(
            !names.contains(&"send_sidekick".to_string()),
            "send_sidekick must not be available to sidekicks (no recursion): {names:?}"
        );
    }

    #[test]
    fn build_subagent_tools_installs_permission_hook() {
        // Even if a tool slips past the name filter somehow, the
        // PermissionHook installed on the filtered registry should
        // block the call at dispatch time.
        use async_trait::async_trait;

        struct PanicTool;
        #[async_trait]
        impl Tool for PanicTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "bash".into(),
                    description: "fake bash".into(),
                    parameters: json!({"type": "object"}),
                }
            }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<String> {
                panic!("sidekick permission hook did not block bash");
            }
        }

        let mut parent_tools = ToolRegistry::new();
        // Force-install bash INTO the sidekick's registry by going
        // through tool_by_name + register_arc directly; this bypasses
        // the filter so we can verify the hook is the second line of
        // defense.
        parent_tools.register(PanicTool);

        let cast = Arc::new(Cast::builtin());
        let tool = DispatchSubagentTool::new(Arc::clone(&cast), parent_tools.clone(), test_llm_factory());
        let scout = cast.get("scout").unwrap();

        let mut filtered = tool.build_subagent_tools(scout);
        // Hard-inject bash (simulating the filter getting bypassed).
        let bash = parent_tools.tool_by_name("bash").expect("bash exists in parent");
        filtered.register_arc(bash);

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = runtime.block_on(filtered.execute(&crate::tool::ToolCall {
            id: "call-1".into(),
            name: "bash".into(),
            arguments: json!({"command": "rm -rf /"}),
        }));

        assert!(result.is_error, "permission hook must block bash for scout");
        assert!(
            result.content.contains("agent 'scout' is not permitted to call 'bash'"),
            "unexpected error: {}",
            result.content
        );
    }
}
