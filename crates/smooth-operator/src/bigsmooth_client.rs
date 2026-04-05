//! BigSmoothReporter — reverse client for operators to communicate back to Big Smooth.
//!
//! Operators running inside sandboxes use this to report progress, token deltas,
//! tool call results, and receive steering commands from the orchestrator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

// ---------------------------------------------------------------------------
// Reporter → Big Smooth events
// ---------------------------------------------------------------------------

/// Events an operator sends back to Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReporterEvent {
    Progress {
        phase: String,
        message: String,
    },
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
    AccessRequest {
        resource_type: String,
        resource: String,
        reason: String,
    },
    NarcAlert {
        severity: String,
        category: String,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Big Smooth → Operator control events
// ---------------------------------------------------------------------------

/// Events Big Smooth sends to an operator for steering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlEvent {
    Steer { action: String, message: Option<String> },
    Cancel,
    AccessResponse { approved: bool, reason: String },
    PolicyUpdate { policy_toml: String },
    Heartbeat,
}

// ---------------------------------------------------------------------------
// BigSmoothReporter
// ---------------------------------------------------------------------------

/// Client for operators to communicate back to Big Smooth.
/// Used inside sandboxes to report progress and receive steering commands.
pub struct BigSmoothReporter {
    ws_tx: Option<mpsc::UnboundedSender<String>>,
    control_rx: Option<mpsc::UnboundedReceiver<ControlEvent>>,
    connected: Arc<AtomicBool>,
}

impl BigSmoothReporter {
    /// Create a new reporter (not yet connected).
    #[must_use]
    pub fn new() -> Self {
        Self {
            ws_tx: None,
            control_rx: None,
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Connect to Big Smooth's operator WebSocket endpoint.
    pub async fn connect(&mut self, bigsmooth_url: &str) -> anyhow::Result<()> {
        let ws_url = bigsmooth_url.replace("http://", "ws://").replace("https://", "wss://");
        let ws_url = format!("{}/ws/operator", ws_url.trim_end_matches('/'));

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to Big Smooth: {e}"))?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<String>();
        let (control_tx, control_rx) = mpsc::unbounded_channel::<ControlEvent>();

        let connected = Arc::clone(&self.connected);
        connected.store(true, Ordering::SeqCst);

        // Write loop: send reporter events to Big Smooth
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

        // Read loop: receive control events from Big Smooth
        let connected_read = Arc::clone(&connected);
        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_source.next().await {
                let text = match msg {
                    tungstenite::Message::Text(t) => t.to_string(),
                    tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                if let Ok(event) = serde_json::from_str::<ControlEvent>(&text) {
                    if control_tx.send(event).is_err() {
                        break;
                    }
                }
            }
            connected_read.store(false, Ordering::SeqCst);
        });

        self.ws_tx = Some(send_tx);
        self.control_rx = Some(control_rx);

        Ok(())
    }

    /// Report an event to Big Smooth.
    ///
    /// # Errors
    /// Returns error if not connected or send fails.
    pub async fn report(&self, event: ReporterEvent) -> anyhow::Result<()> {
        let tx = self.ws_tx.as_ref().ok_or_else(|| anyhow::anyhow!("Not connected to Big Smooth"))?;
        let json = serde_json::to_string(&event)?;
        tx.send(json).map_err(|e| anyhow::anyhow!("Failed to send to Big Smooth: {e}"))
    }

    /// Receive the next control event from Big Smooth (blocking).
    pub async fn recv_control(&mut self) -> Option<ControlEvent> {
        if let Some(rx) = self.control_rx.as_mut() {
            rx.recv().await
        } else {
            None
        }
    }

    /// Try to receive a control event without blocking (returns immediately).
    pub fn try_recv_control(&mut self) -> Option<ControlEvent> {
        if let Some(rx) = self.control_rx.as_mut() {
            rx.try_recv().ok()
        } else {
            None
        }
    }

    /// Returns `true` if the WebSocket is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Disconnect from Big Smooth.
    pub fn disconnect(&mut self) {
        self.ws_tx.take();
        self.control_rx.take();
        self.connected.store(false, Ordering::SeqCst);
    }
}

impl Default for BigSmoothReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for BigSmoothReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigSmoothReporter").field("connected", &self.is_connected()).finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporter_event_token_delta_serialization() {
        let event = ReporterEvent::TokenDelta { content: "hello world".into() };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"TokenDelta"#));
        assert!(json.contains(r#""content":"hello world"#));

        // Roundtrip
        let parsed: ReporterEvent = serde_json::from_str(&json).expect("deserialize");
        if let ReporterEvent::TokenDelta { content } = parsed {
            assert_eq!(content, "hello world");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn control_event_steer_serialization() {
        let event = ControlEvent::Steer {
            action: "pause".into(),
            message: Some("wait for review".into()),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"Steer"#));
        assert!(json.contains(r#""action":"pause"#));
        assert!(json.contains(r#""message":"wait for review"#));

        // Roundtrip
        let parsed: ControlEvent = serde_json::from_str(&json).expect("deserialize");
        if let ControlEvent::Steer { action, message } = parsed {
            assert_eq!(action, "pause");
            assert_eq!(message.as_deref(), Some("wait for review"));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn bigsmooth_reporter_new_starts_disconnected() {
        let reporter = BigSmoothReporter::new();
        assert!(!reporter.is_connected());
        assert!(reporter.ws_tx.is_none());
        assert!(reporter.control_rx.is_none());
    }

    #[test]
    fn all_reporter_event_variants_serialize() {
        let events: Vec<ReporterEvent> = vec![
            ReporterEvent::Progress {
                phase: "assess".into(),
                message: "analyzing".into(),
            },
            ReporterEvent::TokenDelta { content: "hi".into() },
            ReporterEvent::ToolCallStart {
                tool_name: "bash".into(),
                arguments: "ls".into(),
            },
            ReporterEvent::ToolCallComplete {
                tool_name: "bash".into(),
                result: "files".into(),
                is_error: false,
                duration_ms: 42,
            },
            ReporterEvent::TaskComplete { iterations: 5, cost_usd: 0.03 },
            ReporterEvent::TaskError { message: "oops".into() },
            ReporterEvent::CheckpointSaved { checkpoint_id: "cp-1".into() },
            ReporterEvent::AccessRequest {
                resource_type: "network".into(),
                resource: "api.openai.com".into(),
                reason: "LLM call".into(),
            },
            ReporterEvent::NarcAlert {
                severity: "high".into(),
                category: "secret".into(),
                message: "found API key".into(),
            },
        ];

        for (i, event) in events.iter().enumerate() {
            let json = serde_json::to_string(event);
            assert!(json.is_ok(), "variant {i} failed to serialize: {event:?}");
            let json = json.expect("serialize");
            assert!(json.contains(r#""type":"#), "variant {i} missing type tag");

            // Verify roundtrip
            let parsed: ReporterEvent = serde_json::from_str(&json).unwrap_or_else(|e| panic!("variant {i} failed to deserialize: {e}"));
            let json2 = serde_json::to_string(&parsed).expect("re-serialize");
            assert_eq!(json, json2, "roundtrip mismatch for variant {i}");
        }
    }

    #[test]
    fn all_control_event_variants_deserialize() {
        let cases = vec![
            r#"{"type":"Steer","action":"resume","message":null}"#,
            r#"{"type":"Cancel"}"#,
            r#"{"type":"AccessResponse","approved":true,"reason":"allowed"}"#,
            r#"{"type":"PolicyUpdate","policy_toml":"[network]\nallow_all = false"}"#,
            r#"{"type":"Heartbeat"}"#,
        ];

        for (i, json) in cases.iter().enumerate() {
            let result = serde_json::from_str::<ControlEvent>(json);
            assert!(result.is_ok(), "case {i} failed to deserialize: {json} — error: {}", result.unwrap_err());
        }
    }
}
