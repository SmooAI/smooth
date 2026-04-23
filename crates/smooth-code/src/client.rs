//! BigSmoothClient — WebSocket client for smooth-code to talk to Big Smooth.
//!
//! Connects to the Big Smooth `/ws` endpoint, sends [`ClientEvent`]s, and
//! receives [`ServerEvent`]s.  Auto-starts Big Smooth if it is not already
//! running.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use smooth_operator::ws_resilience::{ConnectionManager, ConnectionState, MessageBuffer, ResiliencyConfig};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

// ---------------------------------------------------------------------------
// Event types (local copies — same JSON shape as smooth-bigsmooth::events)
// ---------------------------------------------------------------------------

/// Events sent from this client to Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    TaskStart {
        message: String,
        model: Option<String>,
        budget: Option<f64>,
        working_dir: Option<String>,
        /// Lead role to run under (`fixer` / `mapper` / `oracle` /
        /// `heckler`). `None` means "use the server default"
        /// (`fixer`). Unknown names surface as a TaskError.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    TaskCancel {
        task_id: String,
    },
    Steer {
        task_id: String,
        action: String,
        message: Option<String>,
    },
    Ping,
}

/// Events received from Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    TokenDelta {
        task_id: String,
        content: String,
    },
    ToolCallStart {
        task_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolCallComplete {
        task_id: String,
        tool_name: String,
        result: String,
        is_error: bool,
        duration_ms: u64,
    },
    TaskComplete {
        task_id: String,
        iterations: u32,
        cost_usd: f64,
    },
    TaskError {
        task_id: String,
        message: String,
    },
    PearlCreated {
        id: String,
        title: String,
    },
    NarcAlert {
        severity: String,
        category: String,
        message: String,
    },
    HealthUpdate {
        healthy: bool,
    },
    Connected {
        session_id: String,
    },
    Pong,
    Error {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// BigSmoothClient
// ---------------------------------------------------------------------------

/// WebSocket client for communicating with Big Smooth.
///
/// Includes connection resiliency: automatic reconnection with exponential
/// backoff, heartbeat keep-alive, and an outbound message buffer for messages
/// sent while disconnected.
pub struct BigSmoothClient {
    url: String,
    ws_tx: Option<mpsc::UnboundedSender<String>>,
    event_rx: Option<mpsc::UnboundedReceiver<ServerEvent>>,
    connected: Arc<AtomicBool>,
    conn_mgr: Arc<ConnectionManager>,
    msg_buffer: Arc<MessageBuffer>,
}

impl BigSmoothClient {
    /// Create a new client targeting the given Big Smooth base URL
    /// (e.g. `"http://localhost:4400"`).
    pub fn new(url: &str) -> Self {
        Self::with_config(url, ResiliencyConfig::default())
    }

    /// Create a new client with custom resiliency configuration.
    pub fn with_config(url: &str, config: ResiliencyConfig) -> Self {
        let buffer_size = config.message_buffer_size;
        Self {
            url: url.trim_end_matches('/').to_string(),
            ws_tx: None,
            event_rx: None,
            connected: Arc::new(AtomicBool::new(false)),
            conn_mgr: Arc::new(ConnectionManager::new(config)),
            msg_buffer: Arc::new(MessageBuffer::new(buffer_size)),
        }
    }

    /// Connect to Big Smooth over WebSocket.
    ///
    /// If Big Smooth is not running, attempts to start it by spawning `th up`
    /// in the background and waiting up to 10 seconds for health.
    ///
    /// On success, spawns a heartbeat task and marks the connection as
    /// `Connected`.  Any messages buffered while disconnected are drained and
    /// sent immediately.
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        self.conn_mgr.set_connecting();
        self.ensure_server().await?;

        let ws_url = self.url.replace("http://", "ws://").replace("https://", "wss://");
        let ws_url = format!("{ws_url}/ws");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await.map_err(|e| {
            self.conn_mgr.disconnected();
            anyhow::anyhow!("WebSocket connection failed: {e}")
        })?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Channel: caller -> WS write loop
        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<String>();
        // Channel: WS read loop -> caller
        let (event_tx, event_rx) = mpsc::unbounded_channel::<ServerEvent>();

        let connected = Arc::clone(&self.connected);

        // Write loop
        tokio::spawn(async move {
            while let Some(text) = send_rx.recv().await {
                if ws_sink.send(tungstenite::Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            let _ = ws_sink.send(tungstenite::Message::Close(None)).await;
        });

        // Read loop — on disconnect, mark state and trigger reconnect
        let connected_read = Arc::clone(&connected);
        let conn_mgr_read = Arc::clone(&self.conn_mgr);
        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_source.next().await {
                let text = match msg {
                    tungstenite::Message::Text(t) => t.to_string(),
                    tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                if let Ok(event) = serde_json::from_str::<ServerEvent>(&text) {
                    if matches!(event, ServerEvent::Connected { .. }) {
                        connected_read.store(true, Ordering::SeqCst);
                    }
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
            }
            connected_read.store(false, Ordering::SeqCst);
            conn_mgr_read.disconnected();
        });

        self.ws_tx = Some(send_tx.clone());
        self.event_rx = Some(event_rx);

        // Wait for Connected event (up to 5s)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.connected.load(Ordering::SeqCst) {
            if tokio::time::Instant::now() >= deadline {
                self.conn_mgr.disconnected();
                anyhow::bail!("Timed out waiting for Connected event from Big Smooth");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Mark connected + reset attempts
        self.conn_mgr.connected();

        // Drain buffered messages
        for msg in self.msg_buffer.drain() {
            let _ = send_tx.send(msg);
        }

        // Spawn heartbeat task
        let hb_tx = send_tx;
        let hb_connected = Arc::clone(&self.connected);
        let hb_interval = self.conn_mgr.config().heartbeat_interval;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(hb_interval).await;
                if !hb_connected.load(Ordering::SeqCst) {
                    break;
                }
                let ping = serde_json::to_string(&ClientEvent::Ping).unwrap_or_default();
                if hb_tx.send(ping).is_err() {
                    break;
                }
            }
        });

        Ok(())
    }

    /// Attempt to reconnect using exponential backoff.
    ///
    /// Returns `Ok(())` once reconnected or `Err` if max attempts exhausted.
    pub async fn reconnect(&mut self) -> anyhow::Result<()> {
        while self.conn_mgr.should_reconnect() {
            self.conn_mgr.set_reconnecting();
            let attempt = self.conn_mgr.reconnect_attempts();
            let backoff = self.conn_mgr.backoff_duration(attempt.saturating_sub(1));
            tokio::time::sleep(backoff).await;

            match self.connect().await {
                Ok(()) => return Ok(()),
                Err(_) => continue,
            }
        }
        anyhow::bail!("Max reconnect attempts ({}) exhausted", self.conn_mgr.reconnect_attempts())
    }

    /// Send a task start and return a receiver for streaming server events.
    ///
    /// The returned receiver will yield events until the task completes or
    /// errors.  The caller should drain this receiver.
    pub async fn run_task(
        &mut self,
        message: &str,
        model: Option<&str>,
        budget: Option<f64>,
        working_dir: Option<&str>,
        agent: Option<&str>,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<ServerEvent>> {
        let event = ClientEvent::TaskStart {
            message: message.to_string(),
            model: model.map(ToString::to_string),
            budget,
            working_dir: working_dir.map(ToString::to_string),
            agent: agent.map(ToString::to_string),
        };
        self.send(&event).await?;

        // Return a new channel that filters events for this task
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(mut source) = self.event_rx.take() {
            tokio::spawn(async move {
                while let Some(event) = source.recv().await {
                    let is_terminal = matches!(event, ServerEvent::TaskComplete { .. } | ServerEvent::TaskError { .. });
                    if tx.send(event).is_err() {
                        break;
                    }
                    if is_terminal {
                        break;
                    }
                }
                // Put remaining events back? No — we consume the stream for this task.
                drop(source);
            });
        }

        Ok(rx)
    }

    /// Cancel a running task.
    pub async fn cancel_task(&self, task_id: &str) -> anyhow::Result<()> {
        self.send(&ClientEvent::TaskCancel { task_id: task_id.to_string() }).await
    }

    /// Send a steering command to a running task.
    pub async fn steer(&self, task_id: &str, action: &str, message: Option<&str>) -> anyhow::Result<()> {
        self.send(&ClientEvent::Steer {
            task_id: task_id.to_string(),
            action: action.to_string(),
            message: message.map(ToString::to_string),
        })
        .await
    }

    /// Returns `true` if the WebSocket is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Returns the current connection state for UI display.
    pub fn connection_state(&self) -> ConnectionState {
        self.conn_mgr.state()
    }

    /// Send a raw [`ClientEvent`] to Big Smooth.
    ///
    /// If the connection is down, the message is buffered (up to the configured
    /// limit) and will be sent when the connection is re-established.
    pub async fn send(&self, event: &ClientEvent) -> anyhow::Result<()> {
        let json = serde_json::to_string(event)?;

        if let Some(tx) = self.ws_tx.as_ref() {
            if self.connected.load(Ordering::SeqCst) {
                return tx.send(json).map_err(|e| anyhow::anyhow!("Failed to send: {e}"));
            }
        }

        // Disconnected — buffer the message
        if self.msg_buffer.enqueue(json) {
            Ok(())
        } else {
            anyhow::bail!("Message buffer full — cannot queue message while disconnected")
        }
    }

    /// Receive the next server event (blocking).
    pub async fn recv(&mut self) -> Option<ServerEvent> {
        if let Some(rx) = self.event_rx.as_mut() {
            rx.recv().await
        } else {
            None
        }
    }

    /// Ensure Big Smooth is running, starting it if needed.
    async fn ensure_server(&self) -> anyhow::Result<()> {
        let health_url = format!("{}/health", self.url);
        let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;

        if client.get(&health_url).send().await.is_ok_and(|r| r.status().is_success()) {
            return Ok(());
        }

        // Try to start Big Smooth
        let th_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("th"));
        let _child = tokio::process::Command::new(&th_bin)
            .arg("up")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn Big Smooth (th up): {e}"))?;

        // Wait up to 10s for health
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if client.get(&health_url).send().await.is_ok_and(|r| r.status().is_success()) {
                return Ok(());
            }
        }

        anyhow::bail!("Big Smooth failed to start within 10 seconds")
    }
}

impl std::fmt::Debug for BigSmoothClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigSmoothClient")
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
    fn client_event_task_start_serialization() {
        let event = ClientEvent::TaskStart {
            message: "build the thing".into(),
            model: Some("gpt-4".into()),
            budget: Some(1.5),
            working_dir: Some("/tmp".into()),
            agent: Some("mapper".into()),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"TaskStart"#));
        assert!(json.contains(r#""message":"build the thing"#));
        assert!(json.contains(r#""model":"gpt-4"#));
        assert!(json.contains(r#""budget":1.5"#));
        assert!(json.contains(r#""agent":"mapper"#));

        // Roundtrip
        let parsed: ClientEvent = serde_json::from_str(&json).expect("deserialize");
        if let ClientEvent::TaskStart {
            message,
            model,
            budget,
            working_dir,
            agent,
        } = parsed
        {
            assert_eq!(message, "build the thing");
            assert_eq!(model.as_deref(), Some("gpt-4"));
            assert_eq!(budget, Some(1.5));
            assert_eq!(working_dir.as_deref(), Some("/tmp"));
            assert_eq!(agent.as_deref(), Some("mapper"));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn client_event_task_start_accepts_missing_agent() {
        // Back-compat: clients that don't send `agent` should still
        // deserialize (the server defaults to `fixer`).
        let json = r#"{"type":"TaskStart","message":"hi","model":null,"budget":null,"working_dir":null}"#;
        let parsed: ClientEvent = serde_json::from_str(json).expect("deserialize without agent field");
        if let ClientEvent::TaskStart { agent, .. } = parsed {
            assert!(agent.is_none(), "missing agent should deserialize as None");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn server_event_token_delta_deserialization() {
        let json = r#"{"type":"TokenDelta","task_id":"task-1","content":"hello world"}"#;
        let event: ServerEvent = serde_json::from_str(json).expect("deserialize");
        if let ServerEvent::TokenDelta { task_id, content } = event {
            assert_eq!(task_id, "task-1");
            assert_eq!(content, "hello world");
        } else {
            panic!("unexpected variant: {event:?}");
        }
    }

    #[test]
    fn new_sets_correct_url() {
        let client = BigSmoothClient::new("http://localhost:4400");
        assert_eq!(client.url, "http://localhost:4400");

        // Trailing slash stripped
        let client2 = BigSmoothClient::new("http://localhost:4400/");
        assert_eq!(client2.url, "http://localhost:4400");
    }

    #[test]
    fn is_connected_returns_false_before_connect() {
        let client = BigSmoothClient::new("http://localhost:4400");
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn send_serializes_and_sends_via_channel() {
        // Create a client with a manually wired-up channel
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let config = ResiliencyConfig::default();
        let buffer_size = config.message_buffer_size;
        let conn_mgr = Arc::new(ConnectionManager::new(config));
        conn_mgr.connected();
        let client = BigSmoothClient {
            url: "http://localhost:4400".into(),
            ws_tx: Some(tx),
            event_rx: None,
            connected: Arc::new(AtomicBool::new(true)),
            conn_mgr,
            msg_buffer: Arc::new(MessageBuffer::new(buffer_size)),
        };

        let event = ClientEvent::Ping;
        client.send(&event).await.expect("send");

        let received = rx.recv().await.expect("receive");
        assert!(received.contains(r#""type":"Ping"#));

        // Also test TaskCancel
        let cancel = ClientEvent::TaskCancel { task_id: "t-42".into() };
        client.send(&cancel).await.expect("send cancel");
        let received2 = rx.recv().await.expect("receive cancel");
        assert!(received2.contains(r#""task_id":"t-42"#));
    }
}
