//! Conversation sidebar — a togglable overlay listing saved coding
//! sessions so the user can resume one or start a fresh conversation
//! without leaving the TUI. Parity with the daemon PWA's sidebar.
//!
//! Inline-viewport mode has no persistent left pane (finalized
//! messages live in the terminal's own scrollback), so this rides the
//! same overlay-popup pattern as the model picker: while `active`, it
//! owns the keyboard and renders as a left-anchored panel over the
//! viewport. Backed by the existing [`crate::session::SessionManager`]
//! store — no new persistence layer.

use crate::session::SessionSummary;

/// What selecting the current row should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Start a fresh conversation.
    New,
    /// Resume the session with this id.
    Resume(String),
}

/// State for the conversation sidebar overlay.
#[derive(Debug, Default)]
pub struct SessionPickerState {
    /// Whether the sidebar is currently shown (owns the keyboard).
    pub active: bool,
    /// Saved sessions, newest-first (as returned by `SessionManager::list`).
    pub sessions: Vec<SessionSummary>,
    /// Selected row. Row 0 is always the "New conversation" entry;
    /// rows `1..=sessions.len()` map to `sessions[selected - 1]`.
    pub selected: usize,
    /// Id of the session currently loaded in the chat view, so the
    /// list can highlight it. Empty when unknown.
    pub current_session_id: String,
}

impl SessionPickerState {
    /// Create an inactive picker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total selectable rows: the "New" entry plus every session.
    pub fn row_count(&self) -> usize {
        self.sessions.len() + 1
    }

    /// Open the sidebar with a freshly-listed set of sessions. The
    /// cursor starts on the currently-active session when it's in the
    /// list, otherwise on the "New conversation" row.
    pub fn open(&mut self, sessions: Vec<SessionSummary>, current_session_id: &str) {
        self.selected = sessions.iter().position(|s| s.id == current_session_id).map_or(0, |i| i + 1);
        self.sessions = sessions;
        self.current_session_id = current_session_id.to_string();
        self.active = true;
    }

    /// Hide the sidebar.
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Move the cursor up one row (saturating at the top).
    pub fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the cursor down one row (saturating at the last session).
    pub fn select_down(&mut self) {
        if self.selected + 1 < self.row_count() {
            self.selected += 1;
        }
    }

    /// The action for the currently-selected row.
    pub fn selected_action(&self) -> PickerAction {
        if self.selected == 0 {
            PickerAction::New
        } else {
            // `select_down` clamps `selected` to a valid row, so the
            // index is in range; fall back to New defensively.
            self.sessions
                .get(self.selected - 1)
                .map_or(PickerAction::New, |s| PickerAction::Resume(s.id.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn summary(id: &str, minutes_ago: i64) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            title: Some(format!("title-{id}")),
            preview: format!("preview-{id}"),
            message_count: 2,
            updated_at: Utc::now() - Duration::minutes(minutes_ago),
        }
    }

    #[test]
    fn open_selects_current_session_row() {
        let mut p = SessionPickerState::new();
        p.open(vec![summary("a", 1), summary("b", 5)], "b");
        // Row 0 = New, row 1 = a, row 2 = b → current "b" is row 2.
        assert!(p.active);
        assert_eq!(p.selected, 2);
        assert_eq!(p.selected_action(), PickerAction::Resume("b".into()));
    }

    #[test]
    fn open_defaults_to_new_when_current_absent() {
        let mut p = SessionPickerState::new();
        p.open(vec![summary("a", 1)], "not-in-list");
        assert_eq!(p.selected, 0);
        assert_eq!(p.selected_action(), PickerAction::New);
    }

    #[test]
    fn navigation_saturates_at_both_ends() {
        let mut p = SessionPickerState::new();
        p.open(vec![summary("a", 1), summary("b", 2)], "");
        // Starts at row 0 (New, since current id empty). Up saturates.
        p.select_up();
        assert_eq!(p.selected, 0);
        p.select_down();
        p.select_down();
        assert_eq!(p.selected, 2); // last session
        p.select_down(); // saturates at last row
        assert_eq!(p.selected, 2);
    }

    #[test]
    fn row_zero_is_always_new() {
        let mut p = SessionPickerState::new();
        p.open(vec![summary("a", 1)], "a");
        p.select_up();
        p.select_up();
        assert_eq!(p.selected, 0);
        assert_eq!(p.selected_action(), PickerAction::New);
    }

    #[test]
    fn empty_store_only_has_new_row() {
        let mut p = SessionPickerState::new();
        p.open(vec![], "");
        assert_eq!(p.row_count(), 1);
        p.select_down();
        assert_eq!(p.selected, 0);
        assert_eq!(p.selected_action(), PickerAction::New);
    }
}
