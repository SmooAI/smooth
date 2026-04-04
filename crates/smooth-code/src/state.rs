//! Centralized application state for the coding TUI.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// The current input mode of the TUI.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Normal mode — keyboard shortcuts active, no text input.
    Normal,
    /// Input mode — typing into the message box.
    #[default]
    Input,
}

/// Role of a chat message sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

impl fmt::Display for ChatRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "You"),
            Self::Assistant => write!(f, "Smooth"),
            Self::System => write!(f, "System"),
        }
    }
}

/// A single chat message in the conversation.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub role: ChatRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl ChatMessage {
    /// Create a new chat message with an auto-generated ID and current timestamp.
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role,
            content: content.into(),
            timestamp: Utc::now(),
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ChatRole::User, content)
    }

    /// Create an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(ChatRole::Assistant, content)
    }

    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(ChatRole::System, content)
    }
}

/// Centralized application state.
#[allow(clippy::struct_excessive_bools)]
pub struct AppState {
    /// Current input mode.
    pub mode: Mode,
    /// Working directory for the coding session.
    pub working_dir: PathBuf,
    /// Unique session identifier.
    pub session_id: String,
    /// Chat message history.
    pub messages: Vec<ChatMessage>,
    /// Current text in the input box.
    pub input: String,
    /// Cursor position within the input string (byte offset).
    pub input_cursor: usize,
    /// Whether the sidebar panel is visible.
    pub sidebar_visible: bool,
    /// Scroll offset for the chat area (lines from bottom).
    pub scroll_offset: usize,
    /// Whether the user has manually scrolled up.
    pub user_scrolled: bool,
    /// Display name of the current LLM model.
    pub model_name: String,
    /// Running total of tokens used this session.
    pub total_tokens: u32,
    /// Flag to exit the main loop.
    pub should_quit: bool,
    /// Whether the agent is currently processing a request.
    pub thinking: bool,
}

impl AppState {
    /// Create a new `AppState` for the given working directory.
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            mode: Mode::default(),
            working_dir,
            session_id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            input: String::new(),
            input_cursor: 0,
            sidebar_visible: false,
            scroll_offset: 0,
            user_scrolled: false,
            model_name: "claude-sonnet-4".to_string(),
            total_tokens: 0,
            should_quit: false,
            thinking: false,
        }
    }

    /// Add a message to the conversation history.
    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        // Auto-scroll to bottom when not manually scrolled
        if !self.user_scrolled {
            self.scroll_offset = 0;
        }
    }

    /// Insert a character at the current cursor position.
    pub fn input_insert(&mut self, ch: char) {
        self.input.insert(self.input_cursor, ch);
        self.input_cursor += ch.len_utf8();
    }

    /// Delete the character before the cursor (backspace).
    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            // Find the previous char boundary
            let prev = self.input[..self.input_cursor].char_indices().next_back().map_or(0, |(i, _)| i);
            self.input.remove(prev);
            self.input_cursor = prev;
        }
    }

    /// Move the input cursor one character to the left.
    pub fn input_move_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor = self.input[..self.input_cursor].char_indices().next_back().map_or(0, |(i, _)| i);
        }
    }

    /// Move the input cursor one character to the right.
    pub fn input_move_right(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input_cursor = self.input[self.input_cursor..]
                .char_indices()
                .nth(1)
                .map_or(self.input.len(), |(i, _)| self.input_cursor + i);
        }
    }

    /// Take the current input, clearing it and resetting the cursor.
    pub fn take_input(&mut self) -> String {
        self.input_cursor = 0;
        std::mem::take(&mut self.input)
    }

    /// Clear the input buffer.
    pub fn input_clear(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_creation_defaults() {
        let state = AppState::new(PathBuf::from("/tmp/test"));
        assert_eq!(state.mode, Mode::Input);
        assert_eq!(state.working_dir, PathBuf::from("/tmp/test"));
        assert!(state.messages.is_empty());
        assert!(state.input.is_empty());
        assert_eq!(state.input_cursor, 0);
        assert!(!state.sidebar_visible);
        assert_eq!(state.scroll_offset, 0);
        assert!(!state.user_scrolled);
        assert!(!state.should_quit);
        assert!(!state.thinking);
        assert_eq!(state.total_tokens, 0);
    }

    #[test]
    fn test_chat_message_constructors() {
        let user = ChatMessage::user("hello");
        assert_eq!(user.role, ChatRole::User);
        assert_eq!(user.content, "hello");

        let assistant = ChatMessage::assistant("hi there");
        assert_eq!(assistant.role, ChatRole::Assistant);
        assert_eq!(assistant.content, "hi there");

        let system = ChatMessage::system("init");
        assert_eq!(system.role, ChatRole::System);
        assert_eq!(system.content, "init");
    }

    #[test]
    fn test_add_message() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        assert!(state.messages.is_empty());

        state.add_message(ChatMessage::user("test"));
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "test");
    }

    #[test]
    fn test_input_insert_and_cursor() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.input_insert('h');
        state.input_insert('i');
        assert_eq!(state.input, "hi");
        assert_eq!(state.input_cursor, 2);
    }

    #[test]
    fn test_input_backspace() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.input_insert('a');
        state.input_insert('b');
        state.input_backspace();
        assert_eq!(state.input, "a");
        assert_eq!(state.input_cursor, 1);

        // Backspace at position 0 does nothing
        state.input_backspace();
        assert!(state.input.is_empty());
        state.input_backspace(); // no panic
    }

    #[test]
    fn test_input_move_left_right() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.input_insert('a');
        state.input_insert('b');
        state.input_insert('c');
        assert_eq!(state.input_cursor, 3);

        state.input_move_left();
        assert_eq!(state.input_cursor, 2);

        state.input_move_left();
        assert_eq!(state.input_cursor, 1);

        state.input_move_right();
        assert_eq!(state.input_cursor, 2);

        // Move left past beginning does nothing
        state.input_cursor = 0;
        state.input_move_left();
        assert_eq!(state.input_cursor, 0);

        // Move right past end does nothing
        state.input_cursor = 3;
        state.input_move_right();
        assert_eq!(state.input_cursor, 3);
    }

    #[test]
    fn test_take_input() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.input_insert('h');
        state.input_insert('i');
        let taken = state.take_input();
        assert_eq!(taken, "hi");
        assert!(state.input.is_empty());
        assert_eq!(state.input_cursor, 0);
    }

    #[test]
    fn test_input_clear() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.input_insert('x');
        state.input_clear();
        assert!(state.input.is_empty());
        assert_eq!(state.input_cursor, 0);
    }

    #[test]
    fn test_chat_role_display() {
        assert_eq!(format!("{}", ChatRole::User), "You");
        assert_eq!(format!("{}", ChatRole::Assistant), "Smooth");
        assert_eq!(format!("{}", ChatRole::System), "System");
    }

    #[test]
    fn test_mode_default() {
        assert_eq!(Mode::default(), Mode::Input);
    }
}
