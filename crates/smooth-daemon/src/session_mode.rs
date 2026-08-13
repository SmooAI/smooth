//! Per-conversation execution mode — **Plan** (read-only) vs **Auto** (execute).
//!
//! Big Smooth's two real modes (the ones `shift+tab` toggles in `th code` and
//! the Plan/Auto chip toggles on the desktop/mobile faces). The mode is keyed by
//! the operator's per-turn `conversation_id`, so two conversations are
//! independent and a conversation's mode survives across turns. Unset ⇒ Auto.
//!
//! **Plan mode is a read-only guarantee.** When a conversation is in Plan, the
//! daemon's `ToolProvider` filters every mutating tool out of the per-turn tool
//! set (see `operator::tools_for` + [`PLAN_READONLY_TOOLS`]), so a Plan turn
//! *cannot* obtain `edit_file` / `bash` / `send_file` / … — the model never even
//! sees them. The agent researches and calls `present_plan`; the user accepts
//! (which flips the conversation to Auto and it executes) or sends revisions.
//!
//! The store is shared (cheap `Arc` clone) between the `ToolProvider` and the
//! `/api/session/mode` route — same shape as [`smooth_tools::SessionCwd`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// A conversation's execution mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Read-only: mutating tools are filtered out; the agent proposes a plan.
    Plan,
    /// Normal execution — the daemon's default posture.
    #[default]
    Auto,
}

impl Mode {
    /// Parse a wire value (`"plan"` / `"auto"`, case-insensitive). Anything else
    /// ⇒ `None`, so a bad value from a face is rejected rather than silently
    /// treated as Auto (which could downgrade a Plan-mode read-only guarantee).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "plan" => Some(Self::Plan),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// The wire string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Auto => "auto",
        }
    }
}

/// Per-conversation mode store. Cheap to clone (the map is behind an `Arc`) so
/// the `ToolProvider` and the daemon's HTTP route share one store.
#[derive(Clone, Default)]
pub struct SessionModes {
    map: Arc<Mutex<HashMap<String, Mode>>>,
}

impl SessionModes {
    /// An empty store — every conversation defaults to [`Mode::Auto`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The conversation's mode, or [`Mode::Auto`] when unset.
    #[must_use]
    pub fn get(&self, session: &str) -> Mode {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .copied()
            .unwrap_or_default()
    }

    /// Set the conversation's mode.
    pub fn set(&self, session: &str, mode: Mode) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.to_string(), mode);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn unset_defaults_to_auto() {
        let m = SessionModes::new();
        assert_eq!(m.get("c1"), Mode::Auto);
    }

    #[test]
    fn set_and_get_roundtrips_per_conversation() {
        let m = SessionModes::new();
        m.set("c1", Mode::Plan);
        assert_eq!(m.get("c1"), Mode::Plan);
        assert_eq!(m.get("c2"), Mode::Auto, "conversations are independent");
        m.set("c1", Mode::Auto);
        assert_eq!(m.get("c1"), Mode::Auto);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(Mode::parse("plan"), Some(Mode::Plan));
        assert_eq!(Mode::parse("AUTO"), Some(Mode::Auto));
        assert_eq!(Mode::parse(" Plan "), Some(Mode::Plan));
        assert_eq!(Mode::parse("bypass"), None, "an unknown mode is rejected, not coerced to Auto");
        assert_eq!(Mode::parse(""), None);
    }

    #[test]
    fn wire_roundtrip() {
        for m in [Mode::Plan, Mode::Auto] {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
        }
        // serde uses the same lowercase spelling.
        assert_eq!(serde_json::to_string(&Mode::Plan).unwrap(), "\"plan\"");
    }
}
