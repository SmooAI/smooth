//! `ToolLogHook`: one INFO line when a tool call starts and one when it ends
//! (pearl th-5d48ca).
//!
//! On 2026-09-23 a turn took 2m18s and "struggled with th tools", and the
//! daemon log could not say which tool ran, how long it took, or whether it
//! failed: nothing about tool calls was logged at INFO. This hook fixes that.
//!
//! **What is logged:** the tool name, the call id, the argument KEY names, the
//! duration, and the outcome (`ok` / `error` plus the error's leading category,
//! e.g. `blocked by hook`). **What is never logged:** argument values and result
//! content. Those carry message bodies, file contents, shell commands and
//! secrets, and this hook runs before Narc's redaction.
//!
//! Installed FIRST on the engine's hook seam, so every attempted call gets its
//! start line even when a later hook (the permission gate, Narc) blocks it. A
//! blocked call gets no end line, because the engine skips post-hooks for a
//! call a pre-hook refused. "Start with no end" in the log therefore means
//! blocked, or still running.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use smooth_operator::tool::{ToolCall, ToolHook, ToolResult};

/// A start stamp older than this is from a call that never finished (blocked by
/// a later hook, or its turn was cancelled). It is pruned so the map stays
/// bounded over a long-lived daemon.
const STALE_AFTER: Duration = Duration::from_secs(3600);

/// Longest error category logged. The category is the text before the first
/// `:` of an error result (`error`, `blocked by hook`, `unknown tool`), never
/// the detail after it.
const MAX_ERROR_KIND: usize = 32;

#[derive(Default)]
pub struct ToolLogHook {
    started: Mutex<HashMap<String, Instant>>,
}

impl ToolLogHook {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn key(call: &ToolCall) -> String {
        format!("{}\u{1}{}", call.id, call.name)
    }

    /// Record a start, pruning stale entries.
    fn mark_start(&self, call: &ToolCall, now: Instant) {
        let mut started = self.started.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        started.retain(|_, at| now.duration_since(*at) < STALE_AFTER);
        started.insert(Self::key(call), now);
    }

    /// Take the start stamp for a finished call, if one was recorded.
    fn take_start(&self, call: &ToolCall) -> Option<Instant> {
        self.started.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&Self::key(call))
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.started.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }
}

/// The argument key names, comma-joined: shows which shape was called (a
/// `send` with `chat` vs `contact`, say) without a single value.
fn arg_keys(call: &ToolCall) -> String {
    call.arguments
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect::<Vec<_>>().join(","))
        .unwrap_or_default()
}

/// The leading category of an error result: text before the first `:`, capped.
/// Never the detail, which can quote arguments back.
fn error_kind(content: &str) -> String {
    let head = content.split(':').next().unwrap_or("").trim();
    head.chars().take(MAX_ERROR_KIND).collect()
}

#[async_trait]
impl ToolHook for ToolLogHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        self.mark_start(call, Instant::now());
        tracing::info!(tool = %call.name, call_id = %call.id, args = %arg_keys(call), "tool call started");
        Ok(())
    }

    async fn post_call(&self, call: &ToolCall, result: &mut ToolResult) -> anyhow::Result<()> {
        let duration_ms = self
            .take_start(call)
            .map_or(0, |at| u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX));
        if result.is_error {
            tracing::info!(
                tool = %call.name,
                call_id = %call.id,
                duration_ms,
                outcome = "error",
                error_kind = %error_kind(&result.content),
                "tool call finished"
            );
        } else {
            tracing::info!(
                tool = %call.name,
                call_id = %call.id,
                duration_ms,
                outcome = "ok",
                result_bytes = result.content.len(),
                "tool call finished"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }
    }

    fn result(content: &str, is_error: bool) -> ToolResult {
        ToolResult {
            tool_call_id: "c1".into(),
            content: content.into(),
            is_error,
            details: None,
        }
    }

    #[test]
    fn arg_keys_names_the_shape_never_the_values() {
        let c = call(
            "c1",
            "imessage",
            json!({"command": "send", "chat": "iMessage;+;chat9", "text": "my secret plan"}),
        );
        let keys = arg_keys(&c);
        assert!(keys.contains("command") && keys.contains("chat") && keys.contains("text"), "{keys}");
        assert!(
            !keys.contains("secret") && !keys.contains("chat9") && !keys.contains("send"),
            "values must not leak: {keys}"
        );
        assert_eq!(arg_keys(&call("c", "t", json!("not an object"))), "");
    }

    #[test]
    fn error_kind_keeps_the_category_and_drops_the_detail() {
        assert_eq!(error_kind("blocked by hook: narc flagged sk-live-abc123"), "blocked by hook");
        assert_eq!(error_kind("error: sending timed out after 30s"), "error");
        assert_eq!(error_kind("unknown tool: frobnicate"), "unknown tool");
        assert!(error_kind(&"x".repeat(200)).chars().count() <= MAX_ERROR_KIND);
    }

    #[tokio::test]
    async fn a_finished_call_consumes_its_start_stamp() {
        let hook = ToolLogHook::new();
        let c = call("c1", "bash", json!({"command": "ls"}));
        hook.pre_call(&c).await.unwrap();
        assert_eq!(hook.pending(), 1);
        hook.post_call(&c, &mut result("ok", false)).await.unwrap();
        assert_eq!(hook.pending(), 0);
        // An error result goes through the same path.
        hook.pre_call(&c).await.unwrap();
        hook.post_call(&c, &mut result("error: boom", true)).await.unwrap();
        assert_eq!(hook.pending(), 0);
    }

    #[tokio::test]
    async fn the_hook_never_blocks_and_never_rewrites_a_result() {
        let hook = ToolLogHook::new();
        let c = call("c1", "read_file", json!({"path": "/etc/hosts"}));
        assert!(hook.pre_call(&c).await.is_ok());
        let mut r = result("the file body", false);
        hook.post_call(&c, &mut r).await.unwrap();
        assert_eq!(r.content, "the file body");
        assert!(!r.is_error);
        // A post with no recorded start (blocked mid-chain, restart) is harmless.
        let mut orphan = result("x", false);
        hook.post_call(&call("zz", "grep", json!({})), &mut orphan).await.unwrap();
    }

    #[test]
    fn stale_starts_are_pruned_so_the_map_stays_bounded() {
        let hook = ToolLogHook::new();
        let long_ago = Instant::now().checked_sub(STALE_AFTER + Duration::from_secs(1));
        let Some(long_ago) = long_ago else { return }; // a monotonic clock this young can't go back an hour
        hook.mark_start(&call("old", "bash", json!({})), long_ago);
        assert_eq!(hook.pending(), 1);
        hook.mark_start(&call("new", "bash", json!({})), Instant::now());
        assert_eq!(hook.pending(), 1, "the stale entry must be pruned on the next insert");
    }

    #[test]
    fn concurrent_calls_to_the_same_tool_are_tracked_separately() {
        let hook = ToolLogHook::new();
        let now = Instant::now();
        hook.mark_start(&call("a", "grep", json!({})), now);
        hook.mark_start(&call("b", "grep", json!({})), now);
        assert_eq!(hook.pending(), 2);
        assert!(hook.take_start(&call("a", "grep", json!({}))).is_some());
        assert_eq!(hook.pending(), 1);
    }
}
