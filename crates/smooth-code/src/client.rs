//! Wire types the TUI renders from. [`OperatorClient`](crate::operator_client)
//! maps the operator's canonical protocol onto [`ServerEvent`], and the prior
//! conversation history is carried as [`PriorMessage`]s. (The bespoke
//! `BigSmoothClient` + `ClientEvent` that spoke the old :4400 `/ws` were deleted
//! with the rest of the bespoke surface — EPIC th-c89c2a.)

use serde::{Deserialize, Serialize};

/// One message in the TUI's prior-conversation history sent on
/// each `TaskStart`. Mirrors the structure that
/// `smooth_operator::Conversation` expects so the runner can replay
/// the array as native `Message::user` / `Message::assistant` entries
/// without stringifying.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorMessage {
    /// `"user"` or `"assistant"`. Anything else is dropped at the
    /// runner end so a malformed entry can't poison the conversation.
    pub role: String,
    pub content: String,
}

/// Events received from Big Smooth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    TokenDelta {
        task_id: String,
        content: String,
    },
    /// Iteration boundary — fires when the agent enters a new LLM
    /// round. Pearl th-486bd0: clients use this to reset their
    /// streaming-message accumulator so successive iterations don't
    /// pile into one giant assistant bubble.
    LlmIteration {
        task_id: String,
        iteration: u32,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_event_token_delta_round_trips() {
        let json = r#"{"type":"TokenDelta","task_id":"t1","content":"hi"}"#;
        let ev: ServerEvent = serde_json::from_str(json).expect("deserialize");
        assert!(matches!(ev, ServerEvent::TokenDelta { content, .. } if content == "hi"));
    }

    #[test]
    fn prior_message_holds_role_and_content() {
        let m = PriorMessage {
            role: "user".into(),
            content: "hello".into(),
        };
        assert_eq!(m.role, "user");
    }
}
