use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Role of a message participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<crate::tool::ToolCall>,
    pub timestamp: DateTime<Utc>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: vec![],
            timestamp: Utc::now(),
        }
    }

    /// Estimate token count (rough: ~4 chars per token).
    pub fn estimated_tokens(&self) -> usize {
        self.content.len() / 4 + 1
    }
}

/// A conversation is an ordered list of messages with context management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub messages: Vec<Message>,
    pub max_context_tokens: usize,
}

impl Conversation {
    pub fn new(max_context_tokens: usize) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            messages: vec![],
            max_context_tokens,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.messages.push(Message::system(prompt));
        self
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Get messages within the context window, always keeping the system prompt.
    pub fn context_window(&self) -> Vec<&Message> {
        let mut result = Vec::new();
        let mut total_tokens = 0;

        // Always include system messages first
        let system_msgs: Vec<&Message> = self.messages.iter().filter(|m| m.role == Role::System).collect();
        for msg in &system_msgs {
            total_tokens += msg.estimated_tokens();
            result.push(*msg);
        }

        // Add remaining messages from most recent, respecting token limit
        let non_system: Vec<&Message> = self.messages.iter().filter(|m| m.role != Role::System).collect();
        let mut recent = Vec::new();
        for msg in non_system.iter().rev() {
            let tokens = msg.estimated_tokens();
            if total_tokens + tokens > self.max_context_tokens {
                break;
            }
            total_tokens += tokens;
            recent.push(*msg);
        }
        recent.reverse();
        result.extend(recent);

        result
    }

    /// Total estimated tokens in the full conversation.
    pub fn total_tokens(&self) -> usize {
        self.messages.iter().map(Message::estimated_tokens).sum()
    }

    /// Number of messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Get the last assistant message content, if any.
    pub fn last_assistant_content(&self) -> Option<&str> {
        self.messages.iter().rev().find(|m| m.role == Role::Assistant).map(|m| m.content.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        let sys = Message::system("You are helpful");
        assert_eq!(sys.role, Role::System);
        assert_eq!(sys.content, "You are helpful");

        let user = Message::user("Hello");
        assert_eq!(user.role, Role::User);

        let asst = Message::assistant("Hi there");
        assert_eq!(asst.role, Role::Assistant);

        let tool = Message::tool_result("call-123", "result data");
        assert_eq!(tool.role, Role::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("call-123"));
    }

    #[test]
    fn conversation_basics() {
        let mut conv = Conversation::new(100_000).with_system_prompt("Be helpful");
        assert_eq!(conv.len(), 1);
        assert!(!conv.is_empty());

        conv.push(Message::user("Hello"));
        conv.push(Message::assistant("Hi!"));
        assert_eq!(conv.len(), 3);
        assert_eq!(conv.last_assistant_content(), Some("Hi!"));
    }

    #[test]
    fn context_window_keeps_system() {
        let mut conv = Conversation::new(50).with_system_prompt("System");
        for i in 0..100 {
            conv.push(Message::user(format!("msg {i}")));
        }
        let window = conv.context_window();
        assert_eq!(window[0].role, Role::System);
        assert!(window.len() < conv.len()); // should trim
    }

    #[test]
    fn context_window_small_limit() {
        let mut conv = Conversation::new(10).with_system_prompt("S");
        conv.push(Message::user("A short message"));
        conv.push(Message::user("Another message"));
        let window = conv.context_window();
        assert!(!window.is_empty());
        // System always included
        assert_eq!(window[0].role, Role::System);
    }

    #[test]
    fn token_estimation() {
        let msg = Message::user("Hello world!"); // 12 chars → ~4 tokens
        assert!(msg.estimated_tokens() > 0);
    }

    #[test]
    fn message_serialization() {
        let msg = Message::user("Hello");
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"role\":\"user\""));
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.role, Role::User);
        assert_eq!(parsed.content, "Hello");
    }

    #[test]
    fn conversation_serialization() {
        let conv = Conversation::new(100_000).with_system_prompt("Test");
        let json = serde_json::to_string(&conv).expect("serialize");
        let parsed: Conversation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn empty_conversation() {
        let conv = Conversation::new(100_000);
        assert!(conv.is_empty());
        assert_eq!(conv.total_tokens(), 0);
        assert_eq!(conv.last_assistant_content(), None);
    }
}
