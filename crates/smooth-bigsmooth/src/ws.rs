//! WebSocket handler — real-time events and steering.
//!
//! Protocol:
//!   Server → Client: welcome, heartbeat, event, error
//!   Client → Server: ping, subscribe, steering

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

static CLIENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique client ID.
pub fn next_client_id() -> String {
    let n = CLIENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("client-{n}-{}", chrono::Utc::now().timestamp())
}

/// WebSocket message from server to client.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "welcome")]
    Welcome { client_id: String, connected_clients: u32 },
    #[serde(rename = "heartbeat")]
    Heartbeat { ts: i64 },
    #[serde(rename = "pong")]
    Pong { ts: i64 },
    #[serde(rename = "event")]
    Event { event: serde_json::Value },
    #[serde(rename = "error")]
    Error { message: String },
}

/// WebSocket message from client to server.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "subscribe")]
    Subscribe { topics: Vec<String> },
    #[serde(rename = "steering")]
    Steering { bead_id: String, action: String, message: Option<String> },
}

// TODO: Phase 4 — full axum WebSocket upgrade handler
// For now, the REST API handles everything.
// WebSocket will be added when we integrate with the TUI/web clients.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_id_unique() {
        let id1 = next_client_id();
        let id2 = next_client_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_server_message_serializes() {
        let msg = ServerMessage::Welcome {
            client_id: "test".into(),
            connected_clients: 1,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("welcome"));
    }
}
