//! The session row as clients see it: the `flow.session` payload, reduced to
//! what the shared rules read.

use serde::{Deserialize, Serialize};

/// Session lifecycle state (the wire's `snake_case` spelling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    Working,
    Idle,
    NeedsYou,
    Limited,
    Done,
    Dead,
}

impl SessionState {
    /// `done` / `dead`: nothing runs any more.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Dead)
    }
}

/// One session, as the fleet shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// A harness name (`claude`, `codex`, …) or `shell`.
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pearl_id: Option<String>,
    pub state: SessionState,
    #[serde(default)]
    pub unread: bool,
}

impl Session {
    /// A minimal session, for tests and vectors.
    #[must_use]
    pub fn new(id: &str, kind: &str, state: SessionState) -> Self {
        Self {
            id: id.to_string(),
            kind: kind.to_string(),
            title: String::new(),
            project: String::new(),
            worktree: String::new(),
            branch: None,
            pearl_id: None,
            state,
            unread: false,
        }
    }

    /// Something can still be lost: the session is not `done` / `dead`.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        !self.state.is_terminal()
    }

    /// The project's folder name (`project`'s last path component).
    #[must_use]
    pub fn project_name(&self) -> String {
        crate::title::folder_name(&self.project, "").unwrap_or_else(|| self.project.clone())
    }
}
