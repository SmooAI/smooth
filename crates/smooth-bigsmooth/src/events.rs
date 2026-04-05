//! Typed WebSocket event system for Big Smooth.
//!
//! Defines the strongly-typed client-to-server and server-to-client event
//! enums used over the `/ws` WebSocket channel.

use serde::{Deserialize, Serialize};

/// Events sent from a client to Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    TaskStart {
        message: String,
        model: Option<String>,
        budget: Option<f64>,
        working_dir: Option<String>,
    },
    TaskCancel {
        task_id: String,
    },
    Steer {
        task_id: String,
        action: String,
        message: Option<String>,
    },
    IssueCreate {
        title: String,
        description: Option<String>,
        issue_type: Option<String>,
        priority: Option<u8>,
    },
    IssueUpdate {
        id: String,
        status: Option<String>,
        priority: Option<u8>,
    },
    IssueClose {
        ids: Vec<String>,
    },
    Ping,
}

/// Events sent from Big Smooth to connected clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    // ── Task execution ───────────────────────────────────────
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

    // ── Issues ───────────────────────────────────────────────
    IssueCreated {
        id: String,
        title: String,
    },
    IssueUpdated {
        id: String,
        status: String,
    },

    // ── Security ─────────────────────────────────────────────
    NarcAlert {
        severity: String,
        category: String,
        message: String,
    },

    // ── Telemetry ────────────────────────────────────────────
    Telemetry {
        operator_id: Option<String>,
        tokens_used: u64,
        cost_usd: f64,
        active_tasks: u32,
    },

    // ── Health ───────────────────────────────────────────────
    HealthUpdate {
        healthy: bool,
        components: serde_json::Value,
    },

    // ── Connection ───────────────────────────────────────────
    Pong,
    Error {
        message: String,
    },
    Connected {
        session_id: String,
    },
}

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
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"TaskStart"#));
        assert!(json.contains(r#""message":"build the thing"#));
        assert!(json.contains(r#""model":"gpt-4"#));
        assert!(json.contains(r#""budget":1.5"#));

        // Roundtrip
        let parsed: ClientEvent = serde_json::from_str(&json).expect("deserialize");
        if let ClientEvent::TaskStart {
            message,
            model,
            budget,
            working_dir,
        } = parsed
        {
            assert_eq!(message, "build the thing");
            assert_eq!(model.as_deref(), Some("gpt-4"));
            assert_eq!(budget, Some(1.5));
            assert_eq!(working_dir.as_deref(), Some("/tmp"));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn client_event_ping_serialization() {
        let event = ClientEvent::Ping;
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"Ping"#));

        let parsed: ClientEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(parsed, ClientEvent::Ping));
    }

    #[test]
    fn server_event_token_delta_serialization() {
        let event = ServerEvent::TokenDelta {
            task_id: "task-1".into(),
            content: "hello world".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"TokenDelta"#));
        assert!(json.contains(r#""task_id":"task-1"#));
        assert!(json.contains(r#""content":"hello world"#));
    }

    #[test]
    fn server_event_task_complete_serialization() {
        let event = ServerEvent::TaskComplete {
            task_id: "task-42".into(),
            iterations: 7,
            cost_usd: 0.0042,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(r#""type":"TaskComplete"#));
        assert!(json.contains(r#""iterations":7"#));
        assert!(json.contains("0.0042"));
    }

    #[test]
    fn server_event_roundtrip() {
        let events = vec![
            ServerEvent::TokenDelta {
                task_id: "t1".into(),
                content: "hi".into(),
            },
            ServerEvent::Pong,
            ServerEvent::Connected { session_id: "s1".into() },
            ServerEvent::Error { message: "oops".into() },
            ServerEvent::TaskComplete {
                task_id: "t2".into(),
                iterations: 3,
                cost_usd: 0.01,
            },
            ServerEvent::TaskError {
                task_id: "t3".into(),
                message: "fail".into(),
            },
            ServerEvent::ToolCallStart {
                task_id: "t4".into(),
                tool_name: "bash".into(),
                arguments: "ls".into(),
            },
            ServerEvent::ToolCallComplete {
                task_id: "t4".into(),
                tool_name: "bash".into(),
                result: "files".into(),
                is_error: false,
                duration_ms: 42,
            },
            ServerEvent::IssueCreated {
                id: "i1".into(),
                title: "Bug".into(),
            },
            ServerEvent::IssueUpdated {
                id: "i1".into(),
                status: "done".into(),
            },
            ServerEvent::NarcAlert {
                severity: "high".into(),
                category: "secret".into(),
                message: "found key".into(),
            },
            ServerEvent::Telemetry {
                operator_id: None,
                tokens_used: 1000,
                cost_usd: 0.05,
                active_tasks: 2,
            },
            ServerEvent::HealthUpdate {
                healthy: true,
                components: serde_json::json!({"db": "ok"}),
            },
        ];

        for event in events {
            let json = serde_json::to_string(&event).expect("serialize");
            let parsed: ServerEvent = serde_json::from_str(&json).expect("deserialize");
            // Re-serialize to confirm roundtrip stability
            let json2 = serde_json::to_string(&parsed).expect("re-serialize");
            assert_eq!(json, json2, "roundtrip mismatch for {event:?}");
        }
    }

    #[test]
    fn all_client_event_variants_deserialize() {
        let cases = vec![
            r#"{"type":"TaskStart","message":"do it","model":null,"budget":null,"working_dir":null}"#,
            r#"{"type":"TaskCancel","task_id":"t1"}"#,
            r#"{"type":"Steer","task_id":"t1","action":"pause","message":null}"#,
            r#"{"type":"IssueCreate","title":"Bug","description":null,"issue_type":null,"priority":null}"#,
            r#"{"type":"IssueUpdate","id":"i1","status":"done","priority":null}"#,
            r#"{"type":"IssueClose","ids":["i1","i2"]}"#,
            r#"{"type":"Ping"}"#,
        ];

        for (i, json) in cases.iter().enumerate() {
            let result = serde_json::from_str::<ClientEvent>(json);
            assert!(result.is_ok(), "case {i} failed to deserialize: {json} — error: {}", result.unwrap_err());
        }
    }

    #[test]
    fn all_server_event_variants_serialize() {
        let events: Vec<ServerEvent> = vec![
            ServerEvent::TokenDelta {
                task_id: "t".into(),
                content: "c".into(),
            },
            ServerEvent::ToolCallStart {
                task_id: "t".into(),
                tool_name: "n".into(),
                arguments: "a".into(),
            },
            ServerEvent::ToolCallComplete {
                task_id: "t".into(),
                tool_name: "n".into(),
                result: "r".into(),
                is_error: false,
                duration_ms: 0,
            },
            ServerEvent::TaskComplete {
                task_id: "t".into(),
                iterations: 1,
                cost_usd: 0.0,
            },
            ServerEvent::TaskError {
                task_id: "t".into(),
                message: "m".into(),
            },
            ServerEvent::IssueCreated {
                id: "i".into(),
                title: "t".into(),
            },
            ServerEvent::IssueUpdated {
                id: "i".into(),
                status: "s".into(),
            },
            ServerEvent::NarcAlert {
                severity: "s".into(),
                category: "c".into(),
                message: "m".into(),
            },
            ServerEvent::Telemetry {
                operator_id: None,
                tokens_used: 0,
                cost_usd: 0.0,
                active_tasks: 0,
            },
            ServerEvent::HealthUpdate {
                healthy: true,
                components: serde_json::json!({}),
            },
            ServerEvent::Pong,
            ServerEvent::Error { message: "e".into() },
            ServerEvent::Connected { session_id: "s".into() },
        ];

        for (i, event) in events.iter().enumerate() {
            let json = serde_json::to_string(event);
            assert!(json.is_ok(), "variant {i} failed to serialize: {event:?}");
            let json = json.expect("serialize");
            assert!(json.contains(r#""type":"#), "variant {i} missing type tag");
        }
    }

    #[test]
    fn event_broadcast_channel_works() {
        let (tx, mut rx1) = tokio::sync::broadcast::channel::<ServerEvent>(16);
        let mut rx2 = tx.subscribe();

        let event = ServerEvent::TokenDelta {
            task_id: "t1".into(),
            content: "hello".into(),
        };
        tx.send(event.clone()).expect("send");

        let received1 = rx1.try_recv().expect("rx1");
        let received2 = rx2.try_recv().expect("rx2");

        // Verify both receivers got the event
        let json1 = serde_json::to_string(&received1).expect("ser1");
        let json2 = serde_json::to_string(&received2).expect("ser2");
        assert_eq!(json1, json2);
        assert!(json1.contains("hello"));
    }
}
