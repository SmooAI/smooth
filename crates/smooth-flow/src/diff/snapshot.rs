//! Turn snapshots: the worktree as a git tree at each turn boundary, so the
//! `turn` diff can say what the agent's last turn changed.
//!
//! **When.** The engine watches state transitions (from hooks, a native
//! harness's events, or the pane scraper — all land in `set_state`): moving
//! into `working` with no open turn takes a `start` snapshot; leaving the
//! turn (`idle`, `limited`, `done`, `dead`) with an open turn takes an `end`
//! one. A `needs_you` pause is part of the turn and takes nothing.
//!
//! **Cost.** One `git add --all` into a COPY of the user's index (its stat
//! cache makes unchanged files free) plus `git write-tree`: typically tens to
//! a few hundred milliseconds, paid inside the hook call so the snapshot is
//! taken before the agent's next edit. Storage is git objects only for
//! content not already in the store (changed files' blobs, changed
//! directories' trees) plus one `flow.db` row; the last [`KEEP_SNAPSHOTS`]
//! rows per session are kept. Snapshot objects are unreferenced, so `git gc`
//! may prune them after `gc.pruneExpire` (two weeks); a pruned turn says so.
//!
//! `SMOOTH_FLOW_TURN_SNAPSHOTS=0` turns it off.

use serde::{Deserialize, Serialize};

use crate::store::SessionState;

/// Snapshot rows kept per session (20 turns).
pub const KEEP_SNAPSHOTS: usize = 40;

/// The env switch.
pub const ENV: &str = "SMOOTH_FLOW_TURN_SNAPSHOTS";

/// Which turn boundary a snapshot marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapKind {
    Start,
    End,
}

impl SnapKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }

    /// Parse the stored spelling; anything unknown reads as `end`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        if s == "start" {
            Self::Start
        } else {
            Self::End
        }
    }
}

/// One stored snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub seq: i64,
    pub kind: SnapKind,
    pub tree: String,
    /// RFC3339 UTC.
    pub at: String,
}

/// Whether snapshots are on (`$SMOOTH_FLOW_TURN_SNAPSHOTS` != `0`/`false`/`off`).
#[must_use]
pub fn enabled(env: Option<&str>) -> bool {
    !matches!(env.map(|v| v.trim().to_ascii_lowercase()).as_deref(), Some("0" | "false" | "off" | "no"))
}

/// Is a turn open (the newest snapshot is a `start`)?
#[must_use]
pub fn open_turn(snaps: &[Snapshot]) -> bool {
    snaps.last().is_some_and(|s| s.kind == SnapKind::Start)
}

/// The snapshot a transition into `next` calls for, given whether a turn is open.
#[must_use]
pub const fn boundary(next: SessionState, open: bool) -> Option<SnapKind> {
    match next {
        SessionState::Working if !open => Some(SnapKind::Start),
        SessionState::Idle | SessionState::Limited | SessionState::Done | SessionState::Dead if open => Some(SnapKind::End),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(kind: SnapKind) -> Snapshot {
        Snapshot {
            seq: 1,
            kind,
            tree: "t".into(),
            at: String::new(),
        }
    }

    #[test]
    fn boundaries() {
        assert_eq!(boundary(SessionState::Working, false), Some(SnapKind::Start));
        assert_eq!(boundary(SessionState::Working, true), None, "PreToolUse inside a turn");
        assert_eq!(boundary(SessionState::NeedsYou, true), None, "a pause is part of the turn");
        assert_eq!(boundary(SessionState::Idle, true), Some(SnapKind::End));
        assert_eq!(boundary(SessionState::Limited, true), Some(SnapKind::End));
        assert_eq!(boundary(SessionState::Dead, true), Some(SnapKind::End));
        assert_eq!(boundary(SessionState::Idle, false), None, "SessionStart's idle is not a turn");
        assert_eq!(boundary(SessionState::Starting, false), None);
    }

    #[test]
    fn open_turn_and_switch() {
        assert!(!open_turn(&[]));
        assert!(open_turn(&[snap(SnapKind::End), snap(SnapKind::Start)]));
        assert!(!open_turn(&[snap(SnapKind::Start), snap(SnapKind::End)]));
        assert!(enabled(None));
        assert!(enabled(Some("1")));
        assert!(!enabled(Some("0")));
        assert!(!enabled(Some(" Off ")));
        assert_eq!(SnapKind::parse("start"), SnapKind::Start);
        assert_eq!(SnapKind::parse("end"), SnapKind::End);
        assert_eq!(SnapKind::Start.as_str(), "start");
    }
}
