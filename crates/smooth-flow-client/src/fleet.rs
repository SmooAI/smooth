//! The fleet sidebar's grouping and counts (spec §4).

use serde::{Deserialize, Serialize};

use crate::session::{Session, SessionState};

/// One sidebar group: a project's sessions, or "shells".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub project: String,
    pub sessions: Vec<String>,
}

/// The name of the group plain shells are gathered under.
pub const SHELLS: &str = "shells";

/// Sessions grouped by project name, in first-seen order, with plain shells
/// gathered under [`SHELLS`] **last**, so a shell never splits the project
/// groups (th-a14327). `ordered` is the fleet's order.
#[must_use]
pub fn grouped(ordered: &[Session]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut shells: Vec<String> = Vec::new();
    for s in ordered {
        if s.kind == "shell" {
            shells.push(s.id.clone());
            continue;
        }
        let key = s.project_name();
        match groups.iter_mut().find(|g| g.project == key) {
            Some(g) => g.sessions.push(s.id.clone()),
            None => groups.push(Group {
                project: key,
                sessions: vec![s.id.clone()],
            }),
        }
    }
    if !shells.is_empty() {
        groups.push(Group {
            project: SHELLS.to_string(),
            sessions: shells,
        });
    }
    groups
}

/// The footer counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Counts {
    /// `working` or `starting`.
    pub working: usize,
    /// `needs_you` or `limited`.
    pub needs_you: usize,
    pub done: usize,
    pub idle: usize,
}

#[must_use]
pub fn counts(ordered: &[Session]) -> Counts {
    let mut c = Counts::default();
    for s in ordered {
        match s.state {
            SessionState::Working | SessionState::Starting => c.working += 1,
            SessionState::NeedsYou | SessionState::Limited => c.needs_you += 1,
            SessionState::Done => c.done += 1,
            SessionState::Idle => c.idle += 1,
            SessionState::Dead => {}
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, kind: &str, project: &str, state: SessionState) -> Session {
        Session {
            project: project.into(),
            ..Session::new(id, kind, state)
        }
    }

    #[test]
    fn groups_by_project_in_first_seen_order_with_shells_last() {
        let fleet = [
            s("a", "claude", "/w/smooth", SessionState::Working),
            s("b", "shell", "/w/smooth", SessionState::Idle),
            s("c", "codex", "/w/smooai", SessionState::NeedsYou),
            s("d", "claude", "/w/smooth", SessionState::Done),
            s("e", "shell", "/w/smooai", SessionState::Limited),
        ];
        let g = grouped(&fleet);
        let shape: Vec<(&str, Vec<&str>)> = g
            .iter()
            .map(|g| (g.project.as_str(), g.sessions.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(shape, vec![("smooth", vec!["a", "d"]), ("smooai", vec!["c"]), ("shells", vec!["b", "e"])]);
        assert_eq!(
            counts(&fleet),
            Counts {
                working: 1,
                needs_you: 2,
                done: 1,
                idle: 1
            }
        );
    }

    #[test]
    fn no_shells_no_shells_group_and_only_shells_is_one_group() {
        assert!(grouped(&[s("a", "claude", "/w/x", SessionState::Idle)]).iter().all(|g| g.project != SHELLS));
        let g = grouped(&[s("a", "shell", "/w/x", SessionState::Idle), s("b", "shell", "/w/y", SessionState::Idle)]);
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].project.as_str(), g[0].sessions.len()), (SHELLS, 2));
        assert!(grouped(&[]).is_empty());
    }
}
