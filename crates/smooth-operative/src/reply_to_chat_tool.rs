//! `reply_to_chat` operator tool.
//!
//! Lets the teammate post a `[CHAT:TEAMMATE]` comment on its pearl. When
//! the user has the chat session scoped to this teammate (via the sidebar
//! in the web UI; see Phase 4), Big Smooth's broadcast tap streams these
//! comments to the chat panel as live operator replies. They also persist
//! in the pearl history.
//!
//! Use this for direct conversational replies — short messages, status
//! confirmations, follow-ups to a `[CHAT:USER]` the teammate received.
//! For durable progress milestones use a regular pearl comment with a
//! `[PROGRESS]` prefix instead.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};

use crate::pearl_tools::PearlStoreHandle;

pub struct ReplyToChatTool {
    pub pearl_handle: Arc<PearlStoreHandle>,
    pub pearl_id: String,
}

#[async_trait]
impl Tool for ReplyToChatTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "reply_to_chat".to_string(),
            description: "Send a direct reply to the user when they're chatting with you (i.e. when you received a [CHAT:USER] message via the mailbox). Posts a [CHAT:TEAMMATE] comment that the user's chat panel sees streamed live.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["message"],
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Your reply to the user. Keep it conversational; don't dump full status reports here (use [PROGRESS] comments for those)."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let message = arguments["message"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'message'"))?;
        let body = format!("[CHAT:TEAMMATE] {message}");
        self.pearl_handle
            .store
            .add_comment(&self.pearl_id, &body)
            .map_err(|e| anyhow::anyhow!("posting chat reply to pearl {}: {e}", self.pearl_id))?;
        Ok("Reply posted to chat.".to_string())
    }

    fn is_read_only(&self) -> bool {
        false
    }
}
