//! Session persistence for the Smooth TUI.
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

// ---------------------------------------------------------------------------
// Branchable session — tree-structured conversation history (JSONL on disk)
// ---------------------------------------------------------------------------

/// A single entry in the session tree. Each entry has a unique `id` and an
/// optional `parent_id` that forms the tree structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Unique identifier for this entry.
    pub id: String,
    /// Parent entry id (`None` for the root).
    pub parent_id: Option<String>,
    /// The chat message stored at this node.
    pub message: SerializableMessage,
    /// When this entry was created.
    pub timestamp: DateTime<Utc>,
}

/// A session whose messages form a tree (not just a linear list).
///
/// Internally every message ever appended is kept in `entries`. A movable
/// `current_head` pointer determines which branch is "active".
pub struct BranchableSession {
    /// Session identifier.
    pub id: String,
    /// All entries in insertion order.
    entries: Vec<SessionEntry>,
    /// The `id` of the entry that is currently the active leaf.
    current_head: String,
}

impl BranchableSession {
    /// Create an empty branchable session.
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            entries: Vec::new(),
            current_head: String::new(),
        }
    }

    /// Append a message as a child of the current head.
    ///
    /// Returns the newly created entry's id.
    pub fn append(&mut self, message: &ChatMessage) -> String {
        let entry_id = uuid::Uuid::new_v4().to_string();
        let parent_id = if self.entries.is_empty() { None } else { Some(self.current_head.clone()) };

        let entry = SessionEntry {
            id: entry_id.clone(),
            parent_id,
            message: SerializableMessage::from(message),
            timestamp: Utc::now(),
        };
        self.entries.push(entry);
        self.current_head.clone_from(&entry_id);
        entry_id
    }

    /// Fork from a specific entry — sets `current_head` to `from_entry_id` so
    /// the next `append` will create a sibling branch.
    ///
    /// # Errors
    /// Returns an error if `from_entry_id` does not exist in the session.
    pub fn fork(&mut self, from_entry_id: &str) -> anyhow::Result<()> {
        if !self.entries.iter().any(|e| e.id == from_entry_id) {
            anyhow::bail!("Entry {from_entry_id} not found");
        }
        self.current_head = from_entry_id.to_string();
        Ok(())
    }

    /// Return the linear path from root to the current head.
    pub fn current_path(&self) -> Vec<&SessionEntry> {
        self.path_to(&self.current_head)
    }

    /// Return the linear path from root to the given entry id.
    fn path_to(&self, target_id: &str) -> Vec<&SessionEntry> {
        // Build an index for fast lookup.
        let index: std::collections::HashMap<&str, &SessionEntry> = self.entries.iter().map(|e| (e.id.as_str(), e)).collect();

        let mut path = Vec::new();
        let mut cur = target_id;
        while let Some(entry) = index.get(cur) {
            path.push(*entry);
            match &entry.parent_id {
                Some(pid) => cur = pid.as_str(),
                None => break,
            }
        }
        path.reverse();
        path
    }

    /// Get all entries that have more than one child (branch points).
    pub fn branch_points(&self) -> Vec<&SessionEntry> {
        let mut child_count: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for entry in &self.entries {
            if let Some(pid) = &entry.parent_id {
                *child_count.entry(pid.as_str()).or_insert(0) += 1;
            }
        }
        self.entries
            .iter()
            .filter(|e| child_count.get(e.id.as_str()).copied().unwrap_or(0) > 1)
            .collect()
    }

    /// Navigate to a specific entry, making it the current head.
    ///
    /// # Errors
    /// Returns an error if `entry_id` does not exist in the session.
    pub fn goto(&mut self, entry_id: &str) -> anyhow::Result<()> {
        if !self.entries.iter().any(|e| e.id == entry_id) {
            anyhow::bail!("Entry {entry_id} not found");
        }
        self.current_head = entry_id.to_string();
        Ok(())
    }

    /// Get `ChatMessage`s for the current branch (root to current head).
    pub fn current_messages(&self) -> Vec<ChatMessage> {
        self.current_path().iter().map(|e| e.message.to_chat_message()).collect()
    }

    /// Save the session as JSONL (one JSON object per line).
    ///
    /// # Errors
    /// Returns an error if the file cannot be created or serialization fails.
    pub fn save_jsonl(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use std::io::Write;

        let file = fs::File::create(path)?;
        let mut writer = std::io::BufWriter::new(file);

        // First line: session metadata.
        let meta = serde_json::json!({
            "type": "session_meta",
            "id": self.id,
            "current_head": self.current_head,
        });
        serde_json::to_writer(&mut writer, &meta)?;
        writeln!(writer)?;

        // Remaining lines: one entry per line.
        for entry in &self.entries {
            serde_json::to_writer(&mut writer, entry)?;
            writeln!(writer)?;
        }
        writer.flush()?;
        Ok(())
    }

    /// Load a branchable session from a JSONL file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or is malformed.
    pub fn load_jsonl(path: &std::path::Path) -> anyhow::Result<Self> {
        use std::io::BufRead;

        let file = fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let mut lines = reader.lines();

        // First line is session metadata.
        let meta_line = lines.next().ok_or_else(|| anyhow::anyhow!("Empty JSONL file"))??;
        let meta: serde_json::Value = serde_json::from_str(&meta_line)?;
        let id = meta["id"].as_str().ok_or_else(|| anyhow::anyhow!("Missing session id"))?.to_string();
        let current_head = meta["current_head"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing current_head"))?
            .to_string();

        let mut entries = Vec::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: SessionEntry = serde_json::from_str(&line)?;
            entries.push(entry);
        }

        Ok(Self { id, entries, current_head })
    }

    /// Render the session tree as ASCII art.
    pub fn render_tree(&self) -> String {
        if self.entries.is_empty() {
            return "(empty session)".to_string();
        }

        // Build children map.
        let mut children: std::collections::HashMap<Option<&str>, Vec<&SessionEntry>> = std::collections::HashMap::new();
        for entry in &self.entries {
            children.entry(entry.parent_id.as_deref()).or_default().push(entry);
        }

        let mut output = String::new();
        // Find roots (entries with no parent).
        if let Some(roots) = children.get(&None) {
            for root in roots {
                self.render_subtree(root, &children, &mut output, "", true);
            }
        }
        output
    }

    fn render_subtree(
        &self,
        entry: &SessionEntry,
        children: &std::collections::HashMap<Option<&str>, Vec<&SessionEntry>>,
        output: &mut String,
        prefix: &str,
        is_last: bool,
    ) {
        use std::fmt::Write;

        let connector = if prefix.is_empty() {
            ""
        } else if is_last {
            "└── "
        } else {
            "├── "
        };
        let role = &entry.message.role;
        let short_id = if entry.id.len() > 8 { &entry.id[..8] } else { &entry.id };
        let content_preview = if entry.message.content.len() > 40 {
            format!("{}...", &entry.message.content[..40])
        } else {
            entry.message.content.clone()
        };
        let head_marker = if entry.id == self.current_head { " *" } else { "" };

        writeln!(output, "{prefix}{connector}[{short_id}] {role}: {content_preview}{head_marker}").ok();

        let child_prefix = if prefix.is_empty() {
            String::new()
        } else if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };

        if let Some(kids) = children.get(&Some(entry.id.as_str())) {
            for (i, kid) in kids.iter().enumerate() {
                self.render_subtree(kid, children, output, &child_prefix, i == kids.len() - 1);
            }
        }
    }

    /// Get all entries (read-only access).
    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    /// Get the current head id.
    pub fn current_head_id(&self) -> &str {
        &self.current_head
    }
}

impl Default for BranchableSession {
    fn default() -> Self {
        Self::new()
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

    // -----------------------------------------------------------------------
    // BranchableSession tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_append_creates_entries_with_parent_chain() {
        let mut bs = BranchableSession::new();
        let id1 = bs.append(&ChatMessage::user("msg1"));
        let id2 = bs.append(&ChatMessage::assistant("msg2"));
        let id3 = bs.append(&ChatMessage::user("msg3"));

        assert_eq!(bs.entries().len(), 3);
        assert!(bs.entries()[0].parent_id.is_none(), "root has no parent");
        assert_eq!(bs.entries()[1].parent_id.as_deref(), Some(id1.as_str()));
        assert_eq!(bs.entries()[2].parent_id.as_deref(), Some(id2.as_str()));
        assert_eq!(bs.current_head_id(), id3);
    }

    #[test]
    fn test_current_path_returns_linear_path_from_root() {
        let mut bs = BranchableSession::new();
        let id1 = bs.append(&ChatMessage::user("a"));
        let id2 = bs.append(&ChatMessage::assistant("b"));
        let id3 = bs.append(&ChatMessage::user("c"));

        let path = bs.current_path();
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id, id1);
        assert_eq!(path[1].id, id2);
        assert_eq!(path[2].id, id3);
    }

    #[test]
    fn test_fork_creates_new_branch_from_specified_point() {
        let mut bs = BranchableSession::new();
        let id1 = bs.append(&ChatMessage::user("root"));
        let _id2 = bs.append(&ChatMessage::assistant("branch-a reply"));

        // Fork back to id1 and add a different reply.
        bs.fork(&id1).expect("fork should succeed");
        let id3 = bs.append(&ChatMessage::assistant("branch-b reply"));

        // Current path should be root -> branch-b reply.
        let path = bs.current_path();
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id, id1);
        assert_eq!(path[1].id, id3);

        // Both entries with id2 and id3 should have id1 as parent.
        let children_of_root: Vec<_> = bs.entries().iter().filter(|e| e.parent_id.as_deref() == Some(id1.as_str())).collect();
        assert_eq!(children_of_root.len(), 2);
    }

    #[test]
    fn test_current_messages_returns_correct_branch() {
        let mut bs = BranchableSession::new();
        bs.append(&ChatMessage::user("root"));
        bs.append(&ChatMessage::assistant("reply-a"));

        let fork_point = bs.entries()[0].id.clone();
        bs.fork(&fork_point).unwrap();
        bs.append(&ChatMessage::assistant("reply-b"));

        let msgs = bs.current_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "root");
        assert_eq!(msgs[1].content, "reply-b");
    }

    #[test]
    fn test_branch_points_detects_forks() {
        let mut bs = BranchableSession::new();
        let id1 = bs.append(&ChatMessage::user("root"));
        bs.append(&ChatMessage::assistant("child-a"));

        bs.fork(&id1).unwrap();
        bs.append(&ChatMessage::assistant("child-b"));

        let bps = bs.branch_points();
        assert_eq!(bps.len(), 1);
        assert_eq!(bps[0].id, id1);
    }

    #[test]
    fn test_goto_navigates_and_updates_current_messages() {
        let mut bs = BranchableSession::new();
        let id1 = bs.append(&ChatMessage::user("first"));
        let id2 = bs.append(&ChatMessage::assistant("second"));
        let _id3 = bs.append(&ChatMessage::user("third"));

        // Navigate back to id2.
        bs.goto(&id2).expect("goto should succeed");
        assert_eq!(bs.current_head_id(), id2);

        let msgs = bs.current_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "second");

        // Error on invalid id.
        assert!(bs.goto("nonexistent").is_err());

        // Navigate to id1.
        bs.goto(&id1).unwrap();
        assert_eq!(bs.current_messages().len(), 1);
    }

    #[test]
    fn test_save_and_load_jsonl_roundtrip() {
        let tmp = TempDir::new().expect("create tempdir");
        let path = tmp.path().join("session.jsonl");

        let mut bs = BranchableSession::new();
        bs.append(&ChatMessage::user("hello"));
        bs.append(&ChatMessage::assistant("hi"));

        let fork_point = bs.entries()[0].id.clone();
        bs.fork(&fork_point).unwrap();
        bs.append(&ChatMessage::assistant("alternate hi"));

        bs.save_jsonl(&path).expect("save should succeed");

        let loaded = BranchableSession::load_jsonl(&path).expect("load should succeed");
        assert_eq!(loaded.id, bs.id);
        assert_eq!(loaded.entries().len(), 3);
        assert_eq!(loaded.current_head_id(), bs.current_head_id());

        // Verify the tree structure survived.
        let msgs = loaded.current_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "alternate hi");
    }

    #[test]
    fn test_multiple_branches_coexist() {
        let mut bs = BranchableSession::new();
        let root = bs.append(&ChatMessage::user("root"));

        // Branch A: root -> a1 -> a2
        let a1 = bs.append(&ChatMessage::assistant("a1"));
        let _a2 = bs.append(&ChatMessage::user("a2"));

        // Branch B: root -> b1
        bs.fork(&root).unwrap();
        let b1 = bs.append(&ChatMessage::assistant("b1"));

        // Branch C: root -> a1 -> c1
        bs.fork(&a1).unwrap();
        let c1 = bs.append(&ChatMessage::user("c1"));

        // Total entries: root, a1, a2, b1, c1 = 5
        assert_eq!(bs.entries().len(), 5);

        // Two branch points: root (children: a1, b1) and a1 (children: a2, c1).
        let bps = bs.branch_points();
        assert_eq!(bps.len(), 2);
        let bp_ids: Vec<&str> = bps.iter().map(|e| e.id.as_str()).collect();
        assert!(bp_ids.contains(&root.as_str()));
        assert!(bp_ids.contains(&a1.as_str()));

        // Navigate to branch B.
        bs.goto(&b1).unwrap();
        let msgs = bs.current_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].content, "b1");

        // Navigate to branch C.
        bs.goto(&c1).unwrap();
        let msgs = bs.current_messages();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "root");
        assert_eq!(msgs[1].content, "a1");
        assert_eq!(msgs[2].content, "c1");
    }
}
