//! The fleet sidebar's grouping and counts (spec §4).

use serde::{Deserialize, Serialize};

use crate::session::{Session, SessionState};

/// One sidebar group: a project's sessions, or "shells".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub project: String,
    pub sessions: Vec<String>,
}

/// Sessions grouped by project name, in first-seen order, with plain shells
/// gathered under "shells" wherever they first appear. `ordered` is the
/// fleet's order.
#[must_use]
pub fn grouped(ordered: &[Session]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for s in ordered {
        let key = if s.kind == "shell" { "shells".to_string() } else { s.project_name() };
        match groups.iter_mut().find(|g| g.project == key) {
            Some(g) => g.sessions.push(s.id.clone()),
            None => groups.push(Group {
                project: key,
                sessions: vec![s.id.clone()],
            }),
        }
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
    fn groups_by_project_in_first_seen_order_with_shells_together() {
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
        assert_eq!(shape, vec![("smooth", vec!["a", "d"]), ("shells", vec!["b", "e"]), ("smooai", vec!["c"])]);
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
}
