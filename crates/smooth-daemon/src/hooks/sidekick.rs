//! Run the daemon's host `ToolHook`s on every **sidekick** tool call (pearl
//! th-8d1951).
//!
//! The engine's `send_sidekick` tool (`smooth_operator::cast::DispatchSubagentTool`)
//! builds each sidekick's registry by pulling tool `Arc`s out of the snapshot it
//! was handed (`tool_by_name`) into a FRESH `ToolRegistry`. Registry hooks are
//! not carried over, and the engine has no seam to add host hooks to that inner
//! registry. So a hook installed on the snapshot would be dropped, and a sidekick's
//! `bash` / `write_file` / MCP call skipped the permission gate (and its
//! `DenyPolicy`) and Narc entirely. With the kernel sandbox opt-in (th-efbab1),
//! those two userspace hooks are the whole safety net, so that gap was a hole.
//!
//! The fix lives in the one thing the engine does carry over: the tool `Arc`
//! itself. [`HookedTool`] wraps a tool so its `execute` runs the host hook chain
//! around the real call, mirroring `ToolRegistry::execute` exactly:
//!
//! 1. every hook's `pre_call`, in order; the first `Err` blocks the call and the
//!    later hooks never see it (so the permission gate short-circuits Narc, just
//!    as on a top-level turn);
//! 2. the wrapped tool;
//! 3. every hook's `post_call`, in order, over the mutable result, so Narc's
//!    secret redaction and its effect-based shell guard apply to what the
//!    sidekick's model reads.
//!
//! [`hooked_registry`] builds the snapshot `DispatchSubagentTool` is constructed
//! with out of wrapped tools, so whatever subset the engine filters for a
//! sidekick's clearance is still wrapped. The parent turn's own registry keeps
//! the unwrapped tools, so a top-level call is hooked once, by the server, never
//! twice.
//!
//! **Approvals.** The daemon passes the SAME hook instances it installs on the
//! server, so the permission gate here shares the parent's approver channel. A
//! sidekick runs inside the parent's `send_sidekick` call, while that turn is
//! the server's active approval target, so an `Ask` verdict parks and reaches
//! the user over the parent turn's WS exactly like a top-level `Ask`. Denial,
//! timeout or a closed channel fails closed. A provider built without that
//! channel (tests, ephemeral providers) uses a gate with no approver, where an
//! `Ask` is a denial. Either way an `Ask` never silently passes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use smooth_operator::tool::{Tool, ToolCall, ToolHook, ToolRegistry, ToolResult, ToolSchema};

/// Source of unique call ids for wrapped calls. The engine does not pass the
/// model's call id down to `Tool::execute`, and Narc keys its pre-shell
/// workspace snapshot on the call id (taken in `pre_call`, consumed in
/// `post_call`), so every wrapped call needs an id no concurrent call shares.
static NEXT_CALL: AtomicU64 = AtomicU64::new(0);

fn next_call_id() -> String {
    format!("sidekick-{}", NEXT_CALL.fetch_add(1, Ordering::Relaxed))
}

/// A tool whose every execution runs through a host hook chain. See the module
/// docs for why this exists and how it mirrors `ToolRegistry::execute`.
pub struct HookedTool {
    inner: Arc<dyn Tool>,
    hooks: Arc<[Arc<dyn ToolHook>]>,
}

impl HookedTool {
    /// Wrap `inner` so each call runs `hooks` (in order) around it.
    #[must_use]
    pub fn new(inner: Arc<dyn Tool>, hooks: Arc<[Arc<dyn ToolHook>]>) -> Self {
        Self { inner, hooks }
    }
}

#[async_trait]
impl Tool for HookedTool {
    fn schema(&self) -> ToolSchema {
        self.inner.schema()
    }

    fn is_concurrent_safe(&self) -> bool {
        self.inner.is_concurrent_safe()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let call = ToolCall {
            id: next_call_id(),
            name: self.inner.schema().name,
            arguments,
        };

        for hook in self.hooks.iter() {
            if let Err(e) = hook.pre_call(&call).await {
                // Same wording the engine's registry uses for a blocked call;
                // the sidekick's registry prefixes it with `error: `.
                return Err(anyhow::anyhow!("blocked by hook: {e}"));
            }
        }

        let mut result = match self.inner.execute(call.arguments.clone()).await {
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
        };

        // Post-hooks may rewrite the result (Narc redacts secrets, or replaces
        // it when a shell destroyed workspace data). A post-hook's own error is
        // logged and the result still goes back, as in the engine's registry.
        for hook in self.hooks.iter() {
            if let Err(e) = hook.post_call(&call, &mut result).await {
                tracing::warn!(error = %e, tool = %call.name, "sidekick post-hook failed");
            }
        }

        if result.is_error {
            // The sidekick's registry adds `error: ` back, so strip ours rather
            // than doubling it. The content is the post-hook (redacted) text.
            let content = result.content.strip_prefix("error: ").unwrap_or(&result.content);
            Err(anyhow::anyhow!("{content}"))
        } else {
            Ok(result.content)
        }
    }
}

/// Build the registry handed to `DispatchSubagentTool` as its parent snapshot:
/// every tool in `tools`, each wrapped in a [`HookedTool`] over `hooks`.
///
/// `hooks` must be the daemon's full host chain, in the server's order
/// (tool log, permission gate, Narc), so a sidekick call meets the same checks
/// in the same order as a top-level one.
pub fn hooked_registry<'a>(tools: impl IntoIterator<Item = &'a Arc<dyn Tool>>, hooks: &[Arc<dyn ToolHook>]) -> ToolRegistry {
    let hooks: Arc<[Arc<dyn ToolHook>]> = hooks.iter().cloned().collect();
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.register_arc(Arc::new(HookedTool::new(Arc::clone(tool), Arc::clone(&hooks))) as Arc<dyn Tool>);
    }
    registry
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A tool that records each call's arguments and answers with `output`
    /// (or fails with it, when `fail`). Named whatever the test needs, so it can
    /// stand in for `bash` without running a shell.
    struct FakeTool {
        name: &'static str,
        output: String,
        fail: bool,
        calls: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl FakeTool {
        fn build(name: &'static str, output: &str) -> (Arc<dyn Tool>, Arc<Mutex<Vec<serde_json::Value>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let tool = Self {
                name,
                output: output.to_owned(),
                fail: false,
                calls: Arc::clone(&calls),
            };
            (Arc::new(tool), calls)
        }
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_owned(),
                description: "fake".to_owned(),
                parameters: serde_json::json!({ "type": "object" }),
            }
        }

        fn is_read_only(&self) -> bool {
            true
        }

        async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
            self.calls.lock().unwrap().push(arguments);
            if self.fail {
                anyhow::bail!("{}", self.output)
            }
            Ok(self.output.clone())
        }
    }

    /// Records the order hooks are called in, and blocks when told to.
    struct Recorder {
        label: &'static str,
        log: Arc<Mutex<Vec<String>>>,
        block: bool,
    }

    #[async_trait]
    impl ToolHook for Recorder {
        async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("pre:{}:{}", self.label, call.name));
            if self.block {
                anyhow::bail!("{} says no", self.label);
            }
            Ok(())
        }

        async fn post_call(&self, _call: &ToolCall, result: &mut ToolResult) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("post:{}", self.label));
            result.content.push_str(&format!("+{}", self.label));
            Ok(())
        }
    }

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "llm-1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn hooks_run_in_order_around_the_tool() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let hooks: Vec<Arc<dyn ToolHook>> = vec![
            Arc::new(Recorder {
                label: "gate",
                log: Arc::clone(&log),
                block: false,
            }),
            Arc::new(Recorder {
                label: "narc",
                log: Arc::clone(&log),
                block: false,
            }),
        ];
        let (tool, calls) = FakeTool::build("bash", "out");
        let registry = hooked_registry([&tool], &hooks);

        let res = registry.execute(&call("bash", serde_json::json!({ "command": "ls" }))).await;
        assert!(!res.is_error, "{}", res.content);
        assert_eq!(res.content, "out+gate+narc", "post-hooks rewrite the result, in order");
        assert_eq!(calls.lock().unwrap().len(), 1, "the wrapped tool ran once");
        assert_eq!(*log.lock().unwrap(), vec!["pre:gate:bash", "pre:narc:bash", "post:gate", "post:narc"]);
    }

    #[tokio::test]
    async fn a_blocking_first_hook_short_circuits_the_rest_and_the_tool() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let hooks: Vec<Arc<dyn ToolHook>> = vec![
            Arc::new(Recorder {
                label: "gate",
                log: Arc::clone(&log),
                block: true,
            }),
            Arc::new(Recorder {
                label: "narc",
                log: Arc::clone(&log),
                block: false,
            }),
        ];
        let (tool, calls) = FakeTool::build("bash", "out");
        let registry = hooked_registry([&tool], &hooks);

        let res = registry.execute(&call("bash", serde_json::json!({ "command": "ls" }))).await;
        assert!(res.is_error);
        assert_eq!(res.content, "error: blocked by hook: gate says no");
        assert!(calls.lock().unwrap().is_empty(), "a blocked call never reaches the tool");
        assert_eq!(*log.lock().unwrap(), vec!["pre:gate:bash"], "later hooks never see a blocked call");
    }

    #[tokio::test]
    async fn tool_errors_pass_through_post_hooks_without_a_doubled_prefix() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let hooks: Vec<Arc<dyn ToolHook>> = vec![Arc::new(Recorder {
            label: "narc",
            log: Arc::clone(&log),
            block: false,
        })];
        let calls = Arc::new(Mutex::new(Vec::new()));
        let tool: Arc<dyn Tool> = Arc::new(FakeTool {
            name: "bash",
            output: "exit 1".into(),
            fail: true,
            calls,
        });
        let registry = hooked_registry([&tool], &hooks);

        let res = registry.execute(&call("bash", serde_json::json!({}))).await;
        assert!(res.is_error);
        assert_eq!(res.content, "error: exit 1+narc", "post-hooks see the error; one `error: ` prefix");
    }

    #[test]
    fn wrapper_preserves_schema_and_flags() {
        let (tool, _) = FakeTool::build("grep", "x");
        let wrapped = HookedTool::new(Arc::clone(&tool), Arc::from(Vec::new()));
        assert_eq!(wrapped.schema().name, "grep");
        assert!(wrapped.is_read_only());
        assert!(wrapped.is_concurrent_safe());
    }

    #[test]
    fn call_ids_are_unique() {
        let a = next_call_id();
        let b = next_call_id();
        assert_ne!(a, b);
        assert!(a.starts_with("sidekick-"));
    }

    #[tokio::test]
    async fn every_tool_in_the_snapshot_is_wrapped() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let hooks: Vec<Arc<dyn ToolHook>> = vec![Arc::new(Recorder {
            label: "gate",
            log: Arc::clone(&log),
            block: true,
        })];
        let (a, a_calls) = FakeTool::build("read_file", "a");
        let (b, b_calls) = FakeTool::build("write_file", "b");
        let registry = hooked_registry([&a, &b], &hooks);
        // Pulling a tool OUT of the registry (what the engine's sidekick builder
        // does) yields the wrapper, not the raw tool.
        for name in ["read_file", "write_file"] {
            let tool = registry.tool_by_name(name).expect("registered");
            let err = tool.execute(serde_json::json!({})).await.unwrap_err();
            assert!(err.to_string().contains("blocked by hook"), "{name}: {err}");
        }
        assert!(a_calls.lock().unwrap().is_empty() && b_calls.lock().unwrap().is_empty());
    }
}
