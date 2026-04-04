//! Session persistence for the Smooth Coding TUI.
//!
//! Sessions are stored as JSON files under `~/.smooth/coding-sessions/`.
//! Each session captures the full conversation history, model configuration,
//! and token usage so work can be resumed later.

use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::state::{AppState, ChatMessage, ChatRole};

/// A serializable chat message for session persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl From<&ChatMessage> for SerializableMessage {
    fn from(msg: &ChatMessage) -> Self {
        let role = match msg.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::System => "system",
        }
        .to_string();

        Self {
            id: msg.id.clone(),
            role,
            content: msg.content.clone(),
            timestamp: msg.timestamp,
        }
    }
}

impl SerializableMessage {
    /// Convert back to a [`ChatMessage`].
    pub fn to_chat_message(&self) -> ChatMessage {
        let role = match self.role.as_str() {
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            _ => ChatRole::System,
        };
        let mut msg = ChatMessage::new(role, &self.content);
        msg.id.clone_from(&self.id);
        msg.timestamp = self.timestamp;
        msg
    }
}

/// A persisted coding session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier.
    pub id: String,
    /// Chat messages in the session.
    pub messages: Vec<SerializableMessage>,
    /// Display name of the LLM model used.
    pub model_name: String,
    /// Running total of tokens used.
    pub total_tokens: u32,
    /// When the session was first created.
    pub created_at: DateTime<Utc>,
    /// When the session was last modified.
    pub updated_at: DateTime<Utc>,
}

impl Session {
    /// Build a `Session` from the current [`AppState`].
    pub fn from_state(state: &AppState) -> Self {
        let messages = state.messages.iter().map(SerializableMessage::from).collect();
        let now = Utc::now();
        Self {
            id: state.session_id.clone(),
            messages,
            model_name: state.model_name.clone(),
            total_tokens: state.total_tokens,
            created_at: now,
            updated_at: now,
        }
    }
}

/// A lightweight summary of a session for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Session identifier.
    pub id: String,
    /// First user message, truncated.
    pub preview: String,
    /// Number of messages in the session.
    pub message_count: usize,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// Manages session persistence on disk.
pub struct SessionManager {
    sessions_dir: PathBuf,
}

impl SessionManager {
    /// Create a new `SessionManager`, ensuring the sessions directory exists.
    ///
    /// Defaults to `~/.smooth/coding-sessions/`.
    ///
    /// # Errors
    /// Returns an error if the home directory cannot be determined or the
    /// directory cannot be created.
    pub fn new() -> anyhow::Result<Self> {
        let home = dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let sessions_dir = home.join(".smooth").join("coding-sessions");
        fs::create_dir_all(&sessions_dir)?;
        Ok(Self { sessions_dir })
    }

    /// Create a `SessionManager` with a custom directory (useful for testing).
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created.
    pub fn with_dir(sessions_dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&sessions_dir)?;
        Ok(Self { sessions_dir })
    }

    /// Save a session to disk as `{id}.json`.
    ///
    /// # Errors
    /// Returns an error if serialization or file I/O fails.
    pub fn save(&self, session: &Session) -> anyhow::Result<()> {
        let path = self.sessions_dir.join(format!("{}.json", session.id));
        let json = serde_json::to_string_pretty(session)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Load a session from disk by its ID.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(&self, id: &str) -> anyhow::Result<Session> {
        let path = self.sessions_dir.join(format!("{id}.json"));
        let json = fs::read_to_string(path)?;
        let session: Session = serde_json::from_str(&json)?;
        Ok(session)
    }

    /// List all saved sessions as summaries, sorted by most recently updated first.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be read or a session file is malformed.
    pub fn list(&self) -> anyhow::Result<Vec<SessionSummary>> {
        let mut summaries = Vec::new();

        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let json = fs::read_to_string(&path)?;
                if let Ok(session) = serde_json::from_str::<Session>(&json) {
                    let preview = session.messages.iter().find(|m| m.role == "user").map_or_else(
                        || "(no user messages)".to_string(),
                        |m| {
                            if m.content.len() > 60 {
                                format!("{}...", &m.content[..60])
                            } else {
                                m.content.clone()
                            }
                        },
                    );

                    summaries.push(SessionSummary {
                        id: session.id,
                        preview,
                        message_count: session.messages.len(),
                        updated_at: session.updated_at,
                    });
                }
            }
        }

        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    /// Delete a session file by ID.
    ///
    /// # Errors
    /// Returns an error if the file cannot be removed.
    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        let path = self.sessions_dir.join(format!("{id}.json"));
        fs::remove_file(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::state::AppState;

    fn make_test_session() -> Session {
        let mut state = AppState::new(PathBuf::from("/tmp/test-project"));
        state.session_id = "test-session-123".to_string();
        state.model_name = "gpt-4o".to_string();
        state.total_tokens = 1500;
        state.add_message(ChatMessage::system("Welcome"));
        state.add_message(ChatMessage::user("Hello, how do I write tests?"));
        state.add_message(ChatMessage::assistant("Use #[test] attribute in Rust."));
        Session::from_state(&state)
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let tmp = TempDir::new().expect("create tempdir");
        let mgr = SessionManager::with_dir(tmp.path().to_path_buf()).expect("create manager");

        let session = make_test_session();
        mgr.save(&session).expect("save session");

        let loaded = mgr.load("test-session-123").expect("load session");
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.model_name, "gpt-4o");
        assert_eq!(loaded.total_tokens, 1500);
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.messages[1].content, "Hello, how do I write tests?");
    }

    #[test]
    fn test_list_returns_summaries() {
        let tmp = TempDir::new().expect("create tempdir");
        let mgr = SessionManager::with_dir(tmp.path().to_path_buf()).expect("create manager");

        let session = make_test_session();
        mgr.save(&session).expect("save session");

        let summaries = mgr.list().expect("list sessions");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "test-session-123");
        assert_eq!(summaries[0].message_count, 3);
        assert!(summaries[0].preview.contains("Hello, how do I write tests?"));
    }

    #[test]
    fn test_session_serialization() {
        let session = make_test_session();
        let json = serde_json::to_string(&session).expect("serialize");
        let deserialized: Session = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.id, session.id);
        assert_eq!(deserialized.model_name, session.model_name);
        assert_eq!(deserialized.total_tokens, session.total_tokens);
        assert_eq!(deserialized.messages.len(), session.messages.len());

        // Verify message roles survive round-trip
        assert_eq!(deserialized.messages[0].role, "system");
        assert_eq!(deserialized.messages[1].role, "user");
        assert_eq!(deserialized.messages[2].role, "assistant");
    }

    #[test]
    fn test_delete_session() {
        let tmp = TempDir::new().expect("create tempdir");
        let mgr = SessionManager::with_dir(tmp.path().to_path_buf()).expect("create manager");

        let session = make_test_session();
        mgr.save(&session).expect("save session");
        assert!(mgr.load("test-session-123").is_ok());

        mgr.delete("test-session-123").expect("delete session");
        assert!(mgr.load("test-session-123").is_err());
    }
}
