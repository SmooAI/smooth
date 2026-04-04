//! Human-in-the-loop support for agent tool execution.
//!
//! Provides a [`ConfirmationHook`] that intercepts tool calls matching
//! configurable patterns and requires human approval before proceeding.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;

use crate::tool::{ToolCall, ToolHook};

/// A request sent to a human operator for input or confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HumanRequest {
    /// Ask the human to confirm a tool invocation before it proceeds.
    Confirm {
        tool_name: String,
        arguments: serde_json::Value,
        prompt: String,
    },
    /// Ask the human for free-form input.
    Input { prompt: String },
}

/// The human operator's response to a [`HumanRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HumanResponse {
    /// The human approved the action.
    Approved,
    /// The human denied the action, with an optional reason.
    Denied { reason: String },
    /// The human provided free-form input.
    Input { content: String },
    /// No response was received within the timeout window.
    Timeout,
}

/// A [`ToolHook`] that requires human confirmation for tool calls matching
/// any of the configured glob-like patterns.
///
/// When a matching tool is about to execute, the hook sends a
/// [`HumanRequest::Confirm`] through the channel and waits for a
/// [`HumanResponse`]. If the response is not `Approved`, execution is blocked.
pub struct ConfirmationHook {
    patterns: Vec<String>,
    tx: UnboundedSender<HumanRequest>,
    rx: Arc<Mutex<UnboundedReceiver<HumanResponse>>>,
    timeout: Duration,
}

impl std::fmt::Debug for ConfirmationHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmationHook")
            .field("patterns", &self.patterns)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl ConfirmationHook {
    /// Create a new `ConfirmationHook`.
    ///
    /// * `patterns` — tool name patterns to match (simple substring matching).
    /// * `tx` — sender for outbound [`HumanRequest`] messages.
    /// * `rx` — receiver for inbound [`HumanResponse`] messages.
    /// * `timeout` — how long to wait for a response before treating it as [`HumanResponse::Timeout`].
    pub fn new(patterns: Vec<String>, tx: UnboundedSender<HumanRequest>, rx: Arc<Mutex<UnboundedReceiver<HumanResponse>>>, timeout: Duration) -> Self {
        Self { patterns, tx, rx, timeout }
    }

    /// Returns `true` if the given tool name matches any configured pattern.
    fn matches(&self, tool_name: &str) -> bool {
        self.patterns.iter().any(|p| tool_name.contains(p.as_str()))
    }
}

#[async_trait]
impl ToolHook for ConfirmationHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        if !self.matches(&call.name) {
            return Ok(());
        }

        let request = HumanRequest::Confirm {
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
            prompt: format!("Tool '{}' requires confirmation. Allow?", call.name),
        };

        // Send the request; if the receiver is gone, treat as denied.
        if self.tx.send(request).is_err() {
            anyhow::bail!("human confirmation channel closed");
        }

        // Wait for response with timeout.
        let mut rx = self.rx.lock().await;
        match tokio::time::timeout(self.timeout, rx.recv()).await {
            Ok(Some(HumanResponse::Approved)) => Ok(()),
            Ok(Some(HumanResponse::Denied { reason })) => {
                anyhow::bail!("User denied: {reason}")
            }
            Ok(Some(HumanResponse::Timeout)) => {
                anyhow::bail!("confirmation timeout")
            }
            Ok(Some(HumanResponse::Input { .. })) => {
                anyhow::bail!("unexpected Input response to Confirm request")
            }
            Ok(None) => {
                anyhow::bail!("human response channel closed")
            }
            Err(_elapsed) => {
                anyhow::bail!("confirmation timeout")
            }
        }
    }
}

/// The four endpoints of a human-in-the-loop channel pair.
pub struct HumanChannelPair {
    pub request_tx: UnboundedSender<HumanRequest>,
    pub request_rx: UnboundedReceiver<HumanRequest>,
    pub response_tx: UnboundedSender<HumanResponse>,
    pub response_rx: Arc<Mutex<UnboundedReceiver<HumanResponse>>>,
}

/// Create a pair of channels for human-in-the-loop communication.
///
/// Returns a [`HumanChannelPair`] containing:
/// - `request_tx` / `request_rx` — carry [`HumanRequest`] from the agent to the UI
/// - `response_tx` / `response_rx` — carry [`HumanResponse`] from the UI to the agent
pub fn human_channel() -> HumanChannelPair {
    let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
    let (response_tx, resp_rx) = tokio::sync::mpsc::unbounded_channel();
    HumanChannelPair {
        request_tx,
        request_rx,
        response_tx,
        response_rx: Arc::new(Mutex::new(resp_rx)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_request_confirm_serialization() {
        let req = HumanRequest::Confirm {
            tool_name: "rm_file".into(),
            arguments: serde_json::json!({"path": "/tmp/foo"}),
            prompt: "Allow rm_file?".into(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("Confirm"));
        assert!(json.contains("rm_file"));

        let deserialized: HumanRequest = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            HumanRequest::Confirm { tool_name, .. } => assert_eq!(tool_name, "rm_file"),
            _ => panic!("expected Confirm variant"),
        }
    }

    #[test]
    fn human_response_denied_serialization() {
        let resp = HumanResponse::Denied {
            reason: "too dangerous".into(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(json.contains("Denied"));
        assert!(json.contains("too dangerous"));

        let deserialized: HumanResponse = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            HumanResponse::Denied { reason } => assert_eq!(reason, "too dangerous"),
            _ => panic!("expected Denied variant"),
        }
    }

    #[test]
    fn human_request_response_roundtrip() {
        let requests = vec![
            HumanRequest::Confirm {
                tool_name: "write".into(),
                arguments: serde_json::json!({}),
                prompt: "Allow?".into(),
            },
            HumanRequest::Input { prompt: "Enter value:".into() },
        ];
        for req in &requests {
            let json = serde_json::to_string(req).expect("serialize");
            let back: HumanRequest = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&back).expect("re-serialize");
            assert_eq!(json, json2);
        }

        let responses = vec![
            HumanResponse::Approved,
            HumanResponse::Denied { reason: "no".into() },
            HumanResponse::Input { content: "hello".into() },
            HumanResponse::Timeout,
        ];
        for resp in &responses {
            let json = serde_json::to_string(resp).expect("serialize");
            let back: HumanResponse = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&back).expect("re-serialize");
            assert_eq!(json, json2);
        }
    }

    #[tokio::test]
    async fn confirmation_hook_blocks_matching_pattern() {
        let (req_tx, req_rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let resp_rx = Arc::new(Mutex::new(resp_rx));

        let hook = ConfirmationHook::new(vec!["dangerous".into()], req_tx, Arc::clone(&resp_rx), Duration::from_secs(5));

        let call = ToolCall {
            id: "call-1".into(),
            name: "dangerous_delete".into(),
            arguments: serde_json::json!({}),
        };

        // Spawn a task that approves the request
        let _req_rx = req_rx; // keep alive
        tokio::spawn(async move {
            resp_tx.send(HumanResponse::Approved).expect("send response");
        });

        // Give the spawned task a moment
        tokio::task::yield_now().await;

        let result = hook.pre_call(&call).await;
        assert!(result.is_ok(), "approved tool should pass: {result:?}");
    }

    #[tokio::test]
    async fn non_matching_pattern_passes_through() {
        let (req_tx, _req_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_resp_tx, resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let resp_rx = Arc::new(Mutex::new(resp_rx));

        let hook = ConfirmationHook::new(vec!["dangerous".into()], req_tx, resp_rx, Duration::from_secs(5));

        let call = ToolCall {
            id: "call-2".into(),
            name: "safe_read".into(),
            arguments: serde_json::json!({}),
        };

        let result = hook.pre_call(&call).await;
        assert!(result.is_ok(), "non-matching tool should pass through");
    }

    #[tokio::test]
    async fn timeout_on_no_response() {
        let (req_tx, _req_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_resp_tx, resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let resp_rx = Arc::new(Mutex::new(resp_rx));

        // Very short timeout so the test is fast
        let hook = ConfirmationHook::new(vec!["slow".into()], req_tx, resp_rx, Duration::from_millis(50));

        let call = ToolCall {
            id: "call-3".into(),
            name: "slow_tool".into(),
            arguments: serde_json::json!({}),
        };

        let result = hook.pre_call(&call).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("confirmation timeout"), "error should mention timeout, got: {err}");
    }

    #[tokio::test]
    async fn denied_reason_included_in_error() {
        let (req_tx, _req_rx) = tokio::sync::mpsc::unbounded_channel();
        let (resp_tx, resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let resp_rx = Arc::new(Mutex::new(resp_rx));

        let hook = ConfirmationHook::new(vec!["write".into()], req_tx, Arc::clone(&resp_rx), Duration::from_secs(5));

        let call = ToolCall {
            id: "call-4".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({}),
        };

        // Send denial before the call
        resp_tx.send(HumanResponse::Denied { reason: "not now".into() }).expect("send");

        let result = hook.pre_call(&call).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("User denied"), "error should mention denial, got: {err}");
        assert!(err.contains("not now"), "error should include reason, got: {err}");
    }
}
