//! OperatorClient — WebSocket client for Big Smooth to talk to operators
//! running inside sandboxes.
//!
//! Each operator runs its own WebSocket server; Big Smooth connects to it
//! via this client to assign tasks, steer, and receive streaming events.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

// ---------------------------------------------------------------------------
// Command / Event types
// ---------------------------------------------------------------------------

/// Commands sent from Big Smooth TO an operator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OperatorCommand {
    TaskAssign {
        issue_id: String,
        message: String,
        model: Option<String>,
        policy_toml: String,
    },
    Steer {
        action: String,
        message: Option<String>,
    },
    Cancel,
    Heartbeat,
}

/// Events sent from an operator BACK to Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OperatorEvent {
    TokenDelta {
        content: String,
    },
    ToolCallStart {
        tool_name: String,
        arguments: String,
    },
    ToolCallComplete {
        tool_name: String,
        result: String,
        is_error: bool,
        duration_ms: u64,
    },
    TaskComplete {
        iterations: u32,
        cost_usd: f64,
    },
    TaskError {
        message: String,
    },
    CheckpointSaved {
        checkpoint_id: String,
    },
    NarcAlert {
        severity: String,
        category: String,
        message: String,
    },
    Heartbeat,
}

// ---------------------------------------------------------------------------
// OperatorClient
// ---------------------------------------------------------------------------

/// WebSocket client for Big Smooth to communicate with a single operator
/// running inside a sandbox.
pub struct OperatorClient {
    operator_id: String,
    url: String,
    ws_tx: Option<mpsc::UnboundedSender<String>>,
    event_rx: Option<mpsc::UnboundedReceiver<OperatorEvent>>,
    connected: Arc<AtomicBool>,
}

impl OperatorClient {
    /// Create a new client for the given operator.
    ///
    /// `url` should be the full WebSocket URL, e.g.
    /// `"ws://sandbox-host:9090/ws"`.
    pub fn new(operator_id: &str, url: &str) -> Self {
        Self {
            operator_id: operator_id.to_string(),
            url: url.to_string(),
            ws_tx: None,
            event_rx: None,
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Connect to the operator's WebSocket server.
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.url)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to operator {}: {e}", self.operator_id))?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<String>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<OperatorEvent>();

        let connected = Arc::clone(&self.connected);
        connected.store(true, Ordering::SeqCst);

        // Write loop
        let connected_write = Arc::clone(&connected);
        tokio::spawn(async move {
            while let Some(text) = send_rx.recv().await {
                if ws_sink.send(tungstenite::Message::Text(text.into())).await.is_err() {
                    connected_write.store(false, Ordering::SeqCst);
                    break;
                }
            }
            let _ = ws_sink.send(tungstenite::Message::Close(None)).await;
        });

        // Read loop
        let connected_read = Arc::clone(&connected);
        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_source.next().await {
                let text = match msg {
                    tungstenite::Message::Text(t) => t.to_string(),
                    tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                if let Ok(event) = serde_json::from_str::<OperatorEvent>(&text) {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
            }
            connected_read.store(false, Ordering::SeqCst);
        });

        self.ws_tx = Some(send_tx);
        self.event_rx = Some(event_rx);

        Ok(())
    }

    /// Assign a task to this operator.
    pub async fn assign_task(&self, issue_id: &str, message: &str, model: Option<&str>, policy_toml: &str) -> anyhow::Result<()> {
        self.send_command(&OperatorCommand::TaskAssign {
            issue_id: issue_id.to_string(),
            message: message.to_string(),
            model: model.map(ToString::to_string),
            policy_toml: policy_toml.to_string(),
        })
        .await
    }

    /// Send a steering command.
    pub async fn steer(&self, action: &str, message: Option<&str>) -> anyhow::Result<()> {
        self.send_command(&OperatorCommand::Steer {
            action: action.to_string(),
            message: message.map(ToString::to_string),
        })
        .await
    }

    /// Cancel the current task.
    pub async fn cancel(&self) -> anyhow::Result<()> {
        self.send_command(&OperatorCommand::Cancel).await
    }

    /// Receive the next operator event (blocking).
    pub async fn recv(&mut self) -> Option<OperatorEvent> {
        if let Some(rx) = self.event_rx.as_mut() {
            rx.recv().await
        } else {
            None
        }
    }

    /// Returns `true` if the WebSocket is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Disconnect and clean up.
    pub fn disconnect(&mut self) {
        self.ws_tx.take();
        self.event_rx.take();
        self.connected.store(false, Ordering::SeqCst);
    }

    /// Returns the operator ID.
    pub fn operator_id(&self) -> &str {
        &self.operator_id
    }

    /// Send a command to the operator.
    async fn send_command(&self, cmd: &OperatorCommand) -> anyhow::Result<()> {
        let tx = self
            .ws_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not connected to operator {}", self.operator_id))?;
        let json = serde_json::to_string(cmd)?;
        tx.send(json)
            .map_err(|e| anyhow::anyhow!("Failed to send to operator {}: {e}", self.operator_id))
    }
}

impl std::fmt::Debug for OperatorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorClient")
            .field("operator_id", &self.operator_id)
            .field("url", &self.url)
            .field("connected", &self.is_connected())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_command_task_assign_serialization() {
        let cmd = OperatorCommand::TaskAssign {
            issue_id: "issue-1".into(),
            message: "fix the bug".into(),
            model: Some("claude-sonnet".into()),
            policy_toml: "[network]\nallow_all = false".into(),
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        assert!(json.contains(r#""type":"TaskAssign"#));
        assert!(json.contains(r#""issue_id":"issue-1"#));
        assert!(json.contains(r#""message":"fix the bug"#));
        assert!(json.contains(r#""model":"claude-sonnet"#));

        // Roundtrip
        let parsed: OperatorCommand = serde_json::from_str(&json).expect("deserialize");
        if let OperatorCommand::TaskAssign {
            issue_id,
            message,
            model,
            policy_toml,
        } = parsed
        {
            assert_eq!(issue_id, "issue-1");
            assert_eq!(message, "fix the bug");
            assert_eq!(model.as_deref(), Some("claude-sonnet"));
            assert!(policy_toml.contains("allow_all"));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn operator_event_task_complete_deserialization() {
        let json = r#"{"type":"TaskComplete","iterations":5,"cost_usd":0.042}"#;
        let event: OperatorEvent = serde_json::from_str(json).expect("deserialize");
        if let OperatorEvent::TaskComplete { iterations, cost_usd } = event {
            assert_eq!(iterations, 5);
            assert!((cost_usd - 0.042).abs() < f64::EPSILON);
        } else {
            panic!("unexpected variant: {event:?}");
        }
    }

    #[test]
    fn new_sets_operator_id() {
        let client = OperatorClient::new("op-42", "ws://sandbox:9090/ws");
        assert_eq!(client.operator_id(), "op-42");
        assert_eq!(client.url, "ws://sandbox:9090/ws");
    }

    #[test]
    fn is_connected_returns_false_before_connect() {
        let client = OperatorClient::new("op-1", "ws://localhost:9090/ws");
        assert!(!client.is_connected());
    }

    #[test]
    fn all_operator_event_variants_deserialize() {
        let cases = vec![
            r#"{"type":"TokenDelta","content":"hello"}"#,
            r#"{"type":"ToolCallStart","tool_name":"bash","arguments":"ls"}"#,
            r#"{"type":"ToolCallComplete","tool_name":"bash","result":"files","is_error":false,"duration_ms":42}"#,
            r#"{"type":"TaskComplete","iterations":3,"cost_usd":0.01}"#,
            r#"{"type":"TaskError","message":"oops"}"#,
            r#"{"type":"CheckpointSaved","checkpoint_id":"cp-1"}"#,
            r#"{"type":"NarcAlert","severity":"high","category":"secret","message":"found key"}"#,
            r#"{"type":"Heartbeat"}"#,
        ];

        for (i, json) in cases.iter().enumerate() {
            let result = serde_json::from_str::<OperatorEvent>(json);
            assert!(result.is_ok(), "case {i} failed to deserialize: {json} — error: {}", result.unwrap_err());
        }
    }
}
