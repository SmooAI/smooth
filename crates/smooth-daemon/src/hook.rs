//! The Gate-1 permission [`ToolHook`] — gates every tool call through the
//! [`PermissionEngine`] and, on `Ask`, runs the operator-approval round-trip.
//!
//! Attached to the agent's `ToolRegistry` so the engine calls `pre_call` before
//! each tool runs; returning `Err` blocks the call. This is the *intent* gate;
//! the kernel sandbox (Slice 2) is the enforcement boundary.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use smooth_operator::tool::ToolHook;
use smooth_operator::ToolCall;
use tokio::sync::mpsc::UnboundedSender;

use crate::approval::ApprovalCoordinator;
use crate::permission::{Decision, PermissionEngine};
use crate::wire::ServerEvent;

/// Default operator-approval timeout; on expiry the hook fails **closed** (deny).
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Permission-gating tool hook.
pub struct PermissionHook {
    engine: PermissionEngine,
    coordinator: Arc<ApprovalCoordinator>,
    out: UnboundedSender<ServerEvent>,
    timeout: Duration,
}

impl PermissionHook {
    /// Build a hook for `engine`, emitting approval requests on `out` and
    /// awaiting replies via `coordinator`.
    #[must_use]
    pub fn new(engine: PermissionEngine, coordinator: Arc<ApprovalCoordinator>, out: UnboundedSender<ServerEvent>) -> Self {
        Self {
            engine,
            coordinator,
            out,
            timeout: DEFAULT_APPROVAL_TIMEOUT,
        }
    }

    /// Override the approval timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn ask(&self, call: &ToolCall) -> anyhow::Result<()> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let rx = self.coordinator.register(&request_id);
        let _ = self.out.send(ServerEvent::PermissionRequest {
            request_id: request_id.clone(),
            tool_name: call.name.clone(),
            summary: summarize(call),
        });
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => anyhow::bail!("operator denied {} ({})", call.name, summarize(call)),
            // Receiver error (sender dropped) or timeout → fail closed.
            _ => {
                self.coordinator.forget(&request_id);
                anyhow::bail!("approval timed out for {} (fail-closed deny)", call.name)
            }
        }
    }
}

#[async_trait]
impl ToolHook for PermissionHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        match self.engine.decide(&call.name, &call.arguments) {
            Decision::Allow => Ok(()),
            Decision::Deny => anyhow::bail!("blocked by policy: {} ({})", call.name, summarize(call)),
            Decision::Ask => self.ask(call).await,
        }
    }
}

/// A short, human-readable description of a tool call for the approval prompt.
fn summarize(call: &ToolCall) -> String {
    let a = &call.arguments;
    let s = |k: &str| a.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
    match call.name.as_str() {
        "bash" => s("command"),
        "write_file" | "edit_file" | "read_file" => s("path"),
        "list_files" => format!("pattern={}", s("pattern")),
        "grep" => format!("/{}/", s("pattern")),
        _ => a.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use crate::permission::PermissionMode;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    fn hook(mode: PermissionMode) -> (PermissionHook, tokio::sync::mpsc::UnboundedReceiver<ServerEvent>, Arc<ApprovalCoordinator>) {
        let (out, rx) = tokio::sync::mpsc::unbounded_channel();
        let coord = ApprovalCoordinator::new();
        let h = PermissionHook::new(PermissionEngine::new(mode), Arc::clone(&coord), out).with_timeout(Duration::from_millis(200));
        (h, rx, coord)
    }

    #[tokio::test]
    async fn allow_passes_without_prompt() {
        let (h, mut rx, _c) = hook(PermissionMode::Default);
        assert!(h.pre_call(&call("read_file", json!({"path": "x"}))).await.is_ok());
        assert!(rx.try_recv().is_err(), "no approval request for an allowed tool");
    }

    #[tokio::test]
    async fn deny_blocks() {
        let (h, _rx, _c) = hook(PermissionMode::Default);
        let err = h.pre_call(&call("bash", json!({"command": "rm -rf /"}))).await.unwrap_err();
        assert!(err.to_string().contains("blocked by policy"), "{err}");
    }

    #[tokio::test]
    async fn ask_then_approve_unblocks() {
        let (h, mut rx, coord) = hook(PermissionMode::Default);
        // Run the gated call concurrently; approve when the request arrives.
        let fut = tokio::spawn(async move { h.pre_call(&call("write_file", json!({"path": "f", "content": "x"}))).await });
        // The hook should emit a PermissionRequest.
        let ev = rx.recv().await.expect("a permission request");
        let request_id = match ev {
            ServerEvent::PermissionRequest { request_id, tool_name, .. } => {
                assert_eq!(tool_name, "write_file");
                request_id
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        };
        assert!(coord.resolve(&request_id, true));
        assert!(fut.await.unwrap().is_ok(), "approved call proceeds");
    }

    #[tokio::test]
    async fn ask_then_deny_blocks() {
        let (h, mut rx, coord) = hook(PermissionMode::Default);
        let fut = tokio::spawn(async move { h.pre_call(&call("write_file", json!({"path": "f", "content": "x"}))).await });
        let ev = rx.recv().await.unwrap();
        if let ServerEvent::PermissionRequest { request_id, .. } = ev {
            coord.resolve(&request_id, false);
        }
        assert!(fut.await.unwrap().is_err(), "denied call is blocked");
    }

    #[tokio::test]
    async fn ask_times_out_fail_closed() {
        let (h, _rx, _coord) = hook(PermissionMode::Default);
        // No one answers → the 200ms timeout fires → deny.
        let err = h.pre_call(&call("bash", json!({"command": "npm install"}))).await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
    }
}
