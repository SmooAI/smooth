//! The on-the-wire protocol the daemon speaks to its frontends.
//!
//! These types are **byte-for-byte compatible** with the existing
//! `smooth-bigsmooth` protocol (`smooth-bigsmooth/src/events.rs`) so the
//! `th code` TUI ([`smooth_code::client::BigSmoothClient`]) and the
//! `smooth-web` SPA connect to the new daemon with no protocol changes —
//! pointing them at the daemon is configuration, not code.
//!
//! Both enums use `#[serde(tag = "type")]` with the variant name verbatim
//! (PascalCase), matching the legacy server. The [`tests`] module pins the
//! exact JSON shape so drift from the legacy `events.rs` is caught.
//!
//! > **Cleanup (tracked separately):** the legacy `events.rs`, `smooth-code`,
//! > and this module are three copies of the same contract. Once the daemon is
//! > the default, extract a single `smooth-wire` crate and migrate all three
//! > onto it. Until then, the round-trip tests below are the guard rail.

use serde::{Deserialize, Serialize};

/// A prior conversation turn, replayed into a resumed session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PriorMessage {
    /// `"user"` or `"assistant"`.
    pub role: String,
    /// The message text.
    pub content: String,
}

/// Messages a frontend SENDS to the daemon.
///
/// The full set is defined (not just the subset the daemon acts on yet) so any
/// legacy-client message deserializes cleanly; unhandled variants are
/// acknowledged and ignored by the server until later phases implement them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ClientEvent {
    /// Start a new agent turn.
    TaskStart {
        /// The user's prompt.
        message: String,
        /// Optional model override (else the daemon's configured default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Optional spend cap in USD for this turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<f64>,
        /// Optional working directory override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        /// Optional named agent/role.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        /// Prior turns to replay (session resume).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prior_messages: Vec<PriorMessage>,
    },
    /// Cancel a running task.
    TaskCancel {
        /// The task to cancel.
        task_id: String,
    },
    /// Steer a running task (inject guidance / answer a question).
    Steer {
        /// The task being steered.
        task_id: String,
        /// Steering action discriminator.
        action: String,
        /// Optional steering text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Create a pearl.
    PearlCreate {
        /// Pearl title.
        title: String,
        /// Optional description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Optional type (task/bug/feature).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pearl_type: Option<String>,
        /// Optional priority 0-4.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<u8>,
    },
    /// Update a pearl.
    PearlUpdate {
        /// Pearl id.
        id: String,
        /// New status.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        /// New priority.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<u8>,
    },
    /// Close pearls.
    PearlClose {
        /// Pearl ids to close.
        ids: Vec<String>,
    },
    /// Reply to a [`ServerEvent::PermissionRequest`].
    PermissionReply {
        /// The request being answered.
        request_id: String,
        /// Whether the operator approved.
        allow: bool,
    },
    /// Heartbeat ping.
    Ping,
}

/// Messages the daemon SENDS to a frontend.
///
/// Phase 1 implements the connection + task-execution variants. Pearl,
/// teammate, telemetry, and health variants from the legacy protocol are added
/// as the corresponding subsystems come online in later phases; a frontend
/// simply never receives them until then.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ServerEvent {
    /// Sent immediately on connect, before anything else.
    Connected {
        /// Server-assigned session id for this socket.
        session_id: String,
    },
    /// Heartbeat response.
    Pong,
    /// A connection-level (non-task) error.
    Error {
        /// Human-readable message.
        message: String,
    },
    /// An incremental chunk of assistant output.
    TokenDelta {
        /// The task this delta belongs to.
        task_id: String,
        /// The streamed text fragment.
        content: String,
    },
    /// A new LLM iteration began.
    LlmIteration {
        /// The task.
        task_id: String,
        /// 1-based iteration number.
        iteration: u32,
    },
    /// The agent invoked a tool.
    ToolCallStart {
        /// The task.
        task_id: String,
        /// Tool name.
        tool_name: String,
        /// Serialized JSON arguments.
        arguments: String,
    },
    /// A tool call finished.
    ToolCallComplete {
        /// The task.
        task_id: String,
        /// Tool name.
        tool_name: String,
        /// Truncated result text.
        result: String,
        /// Whether the tool errored.
        is_error: bool,
        /// Wall-clock duration.
        duration_ms: u64,
    },
    /// The task finished normally.
    TaskComplete {
        /// The task.
        task_id: String,
        /// Iterations consumed.
        iterations: u32,
        /// Total cost in USD.
        cost_usd: f64,
    },
    /// The task ended in an error.
    TaskError {
        /// The task.
        task_id: String,
        /// Failure reason.
        message: String,
    },
    /// The agent wants to run something that needs operator approval (Gate-1
    /// `Ask`). The client replies with [`ClientEvent::PermissionReply`].
    PermissionRequest {
        /// Correlates the reply.
        request_id: String,
        /// Tool being gated (e.g. `bash`, `write_file`).
        tool_name: String,
        /// Human-readable description of the action.
        summary: String,
    },
}

/// Map a single engine [`AgentEvent`](smooth_operator::AgentEvent) to the wire
/// [`ServerEvent`] for `task_id`.
///
/// Returns `None` for engine events with no frontend-visible mapping yet
/// (e.g. internal checkpoint/model-resolution events) — the caller simply
/// doesn't forward those. This is the single translation point between the
/// engine's stream and the protocol; keeping it pure makes it exhaustively
/// testable without spinning up an LLM.
#[must_use]
pub fn map_agent_event(task_id: &str, event: &smooth_operator::AgentEvent) -> Option<ServerEvent> {
    use smooth_operator::AgentEvent as E;
    let tid = || task_id.to_owned();
    Some(match event {
        E::TokenDelta { content } => ServerEvent::TokenDelta {
            task_id: tid(),
            content: content.clone(),
        },
        E::LlmRequest { iteration, .. } => ServerEvent::LlmIteration {
            task_id: tid(),
            iteration: *iteration,
        },
        E::ToolCallStart { tool_name, arguments, .. } => ServerEvent::ToolCallStart {
            task_id: tid(),
            tool_name: tool_name.clone(),
            arguments: arguments.clone(),
        },
        E::ToolCallComplete {
            tool_name,
            is_error,
            result,
            duration_ms,
            ..
        } => ServerEvent::ToolCallComplete {
            task_id: tid(),
            tool_name: tool_name.clone(),
            result: result.clone(),
            is_error: *is_error,
            duration_ms: *duration_ms,
        },
        E::Completed { iterations, cost_usd, .. } => ServerEvent::TaskComplete {
            task_id: tid(),
            iterations: *iterations,
            cost_usd: *cost_usd,
        },
        E::MaxIterationsReached { max, .. } => ServerEvent::TaskComplete {
            task_id: tid(),
            iterations: *max,
            cost_usd: 0.0,
        },
        E::Error { message } => ServerEvent::TaskError {
            task_id: tid(),
            message: message.clone(),
        },
        E::BudgetExceeded { spent_usd, limit_usd } => ServerEvent::TaskError {
            task_id: tid(),
            message: format!("budget exceeded: spent ${spent_usd:.4} of ${limit_usd:.4} limit"),
        },
        // Internal / not-yet-surfaced engine events — intentionally not forwarded.
        E::Started { .. }
        | E::LlmResponse { .. }
        | E::CheckpointSaved { .. }
        | E::StreamingComplete
        | E::PhaseStart { .. }
        | E::HumanInputRequired { .. }
        | E::HumanInputReceived { .. }
        | E::DelegationStarted { .. }
        | E::DelegationCompleted { .. }
        | E::PortForwardActive { .. }
        | E::ModelResolved { .. } => return None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn client_task_start_round_trips_and_tags_by_type() {
        let ev = ClientEvent::TaskStart {
            message: "do the thing".into(),
            model: Some("gpt-4o".into()),
            budget: None,
            working_dir: None,
            agent: None,
            prior_messages: vec![],
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "TaskStart");
        assert_eq!(json["message"], "do the thing");
        assert_eq!(json["model"], "gpt-4o");
        // Empty/None fields are omitted (matches legacy skip_serializing_if).
        assert!(json.get("budget").is_none());
        assert!(json.get("prior_messages").is_none());
        let back: ClientEvent = serde_json::from_value(json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn client_ping_is_unit_tagged() {
        assert_eq!(serde_json::to_string(&ClientEvent::Ping).unwrap(), r#"{"type":"Ping"}"#);
        let back: ClientEvent = serde_json::from_str(r#"{"type":"Ping"}"#).unwrap();
        assert_eq!(back, ClientEvent::Ping);
    }

    #[test]
    fn legacy_pearl_message_still_deserializes() {
        // A frontend may send PearlClose; the daemon must parse it even before
        // it acts on it, or the socket read loop would choke.
        let raw = r#"{"type":"PearlClose","ids":["th-1","th-2"]}"#;
        let ev: ClientEvent = serde_json::from_str(raw).unwrap();
        assert_eq!(
            ev,
            ClientEvent::PearlClose {
                ids: vec!["th-1".into(), "th-2".into()]
            }
        );
    }

    #[test]
    fn server_connected_shape_matches_legacy() {
        let ev = ServerEvent::Connected { session_id: "abc".into() };
        assert_eq!(serde_json::to_string(&ev).unwrap(), r#"{"type":"Connected","session_id":"abc"}"#);
    }

    #[test]
    fn server_token_delta_shape_matches_legacy() {
        let ev = ServerEvent::TokenDelta {
            task_id: "t1".into(),
            content: "hi".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "TokenDelta");
        assert_eq!(json["task_id"], "t1");
        assert_eq!(json["content"], "hi");
    }

    #[test]
    fn maps_token_delta() {
        let ae = smooth_operator::AgentEvent::TokenDelta { content: "chunk".into() };
        let se = map_agent_event("t1", &ae).unwrap();
        assert_eq!(
            se,
            ServerEvent::TokenDelta {
                task_id: "t1".into(),
                content: "chunk".into()
            }
        );
    }

    #[test]
    fn maps_tool_call_start_and_complete() {
        let start = smooth_operator::AgentEvent::ToolCallStart {
            iteration: 2,
            tool_name: "bash".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
        };
        assert_eq!(
            map_agent_event("t1", &start).unwrap(),
            ServerEvent::ToolCallStart {
                task_id: "t1".into(),
                tool_name: "bash".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
            }
        );

        let done = smooth_operator::AgentEvent::ToolCallComplete {
            iteration: 2,
            tool_name: "bash".into(),
            is_error: false,
            result: "ok".into(),
            duration_ms: 12,
        };
        assert_eq!(
            map_agent_event("t1", &done).unwrap(),
            ServerEvent::ToolCallComplete {
                task_id: "t1".into(),
                tool_name: "bash".into(),
                result: "ok".into(),
                is_error: false,
                duration_ms: 12,
            }
        );
    }

    #[test]
    fn maps_completion_and_error() {
        let completed = smooth_operator::AgentEvent::Completed {
            agent_id: "a".into(),
            iterations: 5,
            cost_usd: 0.01,
            prompt_tokens: 100,
            completion_tokens: 50,
            cached_tokens: 0,
        };
        assert_eq!(
            map_agent_event("t1", &completed).unwrap(),
            ServerEvent::TaskComplete {
                task_id: "t1".into(),
                iterations: 5,
                cost_usd: 0.01
            }
        );

        let err = smooth_operator::AgentEvent::Error { message: "boom".into() };
        assert_eq!(
            map_agent_event("t1", &err).unwrap(),
            ServerEvent::TaskError {
                task_id: "t1".into(),
                message: "boom".into()
            }
        );
    }

    #[test]
    fn internal_events_are_not_forwarded() {
        let started = smooth_operator::AgentEvent::Started { agent_id: "a".into() };
        assert!(map_agent_event("t1", &started).is_none());
        assert!(map_agent_event("t1", &smooth_operator::AgentEvent::StreamingComplete).is_none());
    }
}
