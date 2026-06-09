//! `ask_smooth` operator tool.
//!
//! Lets a running teammate ask Big Smooth (and through Big Smooth, the user)
//! a question. Two modes:
//!
//! - `urgency = "blocking"`: posts `[QUESTION:TEAMMATE:q-{uuid}] {question}`
//!   to the teammate's pearl, registers a oneshot in the shared
//!   `QuestionRegistry`, and `await`s it with a 5-minute hard cap. The
//!   mailbox poller delivers the answer when it sees a matching
//!   `[ANSWER:USER:q-{uuid}]` or `[ANSWER:SMOOTH:q-{uuid}]` comment. On
//!   timeout the tool returns `"no answer in 5 min — proceeding without"`
//!   so the agent can keep going.
//! - `urgency = "fyi"`: posts the same `[QUESTION:TEAMMATE:q-{uuid}]` and
//!   returns immediately with the question id. Late answers (if any) reach
//!   the agent through the normal injection path as
//!   `InjectedMessageKind::AnswerToQuestion`.
//!
//! See `~/.claude/plans/sorted-orbiting-hummingbird.md` Phase 3.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};

use crate::mailbox::QuestionRegistry;
use crate::pearl_tools::PearlStoreHandle;

const BLOCKING_TIMEOUT: Duration = Duration::from_secs(300);

pub struct AskSmoothTool {
    pub pearl_handle: Arc<PearlStoreHandle>,
    pub questions: Arc<QuestionRegistry>,
    pub pearl_id: String,
}

#[async_trait]
impl Tool for AskSmoothTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "ask_smooth".to_string(),
            description: "Ask Big Smooth (and through it, the user) a question while you're working. Use this when you need clarification, missing context, or a decision that's outside the original task brief. `urgency=\"blocking\"` waits up to 5 minutes for a reply; `urgency=\"fyi\"` returns immediately and any later answer arrives as conversation context.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["question"],
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask. Be concise and specific — the user reads this in chat."
                    },
                    "urgency": {
                        "type": "string",
                        "enum": ["blocking", "fyi"],
                        "default": "blocking",
                        "description": "blocking = wait for an answer (5 min cap); fyi = ask and continue."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let question = arguments["question"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'question'"))?;
        let urgency = arguments.get("urgency").and_then(|v| v.as_str()).unwrap_or("blocking");

        let question_id = format!("q-{}", uuid::Uuid::new_v4().simple().to_string().chars().take(12).collect::<String>());
        let comment_body = format!("[QUESTION:TEAMMATE:{question_id}] {question}");

        // Register BEFORE posting so an unusually fast answer doesn't slip past us.
        let rx = if urgency == "blocking" {
            Some(self.questions.register(question_id.clone()).await)
        } else {
            None
        };

        // Best-effort post; if Dolt is unavailable we surface the error to the
        // agent rather than hang silently.
        self.pearl_handle
            .store
            .add_comment(&self.pearl_id, &comment_body)
            .map_err(|e| anyhow::anyhow!("posting question to pearl {}: {e}", self.pearl_id))?;

        match rx {
            None => Ok(format!(
                "Question queued ({question_id}); the user will see it, but you don't need to wait. Continue with what you can."
            )),
            Some(rx) => match tokio::time::timeout(BLOCKING_TIMEOUT, rx).await {
                Ok(Ok(answer)) => Ok(format!("Answer to {question_id}: {answer}")),
                Ok(Err(_)) => Ok(format!("No answer received for {question_id} (channel closed) — proceeding without.")),
                Err(_) => Ok(format!("No answer received for {question_id} within 5 min — proceeding with best-effort.")),
            },
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        // Each call writes a separate comment with its own uuid; concurrent
        // calls just produce ordered comments. Safe.
        true
    }
}
