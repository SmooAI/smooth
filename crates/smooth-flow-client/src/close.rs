//! What `closePane` does and whether it asks first (spec §5).

use serde::{Deserialize, Serialize};

use crate::session::{Session, SessionState};

/// What a close takes with it, decided by where the focused pane sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One of several panes in the tab.
    Pane,
    /// The last pane of a tab that is not the last tab: the tab goes.
    Tab,
    /// The last pane of the last tab: it empties. The window never closes.
    Last,
}

impl Scope {
    /// The scope for a tab of `panes` panes among `tabs` tabs.
    #[must_use]
    pub const fn of(panes: usize, tabs: usize) -> Self {
        if panes > 1 {
            Self::Pane
        } else if tabs > 1 {
            Self::Tab
        } else {
            Self::Last
        }
    }

    /// The noun the dialog uses ("Close Pane", "Close Tab").
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Pane | Self::Last => "Pane",
            Self::Tab => "Tab",
        }
    }
}

/// What the close dialog offers, when there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub title: String,
    /// The safe answer: the view goes, the session keeps running.
    pub close_title: String,
    /// The destructive answer, always "End Session". Cancel is the default.
    pub kill_title: String,
}

/// Whether a live session has something running worth asking about: any live
/// agent (it holds context), or a shell that isn't sitting at a prompt.
#[must_use]
pub fn has_running_process(session: &Session) -> bool {
    session.is_live() && (session.kind != "shell" || session.state != SessionState::Idle)
}

/// The close decision: `None` closes at once, `Some` asks first.
///
/// Nothing is asked for an empty pane, a finished session, a shell at a
/// prompt, a session still shown in another pane or tab, or when the user
/// turned confirmation off.
#[must_use]
pub fn decide(session: Option<&Session>, scope: Scope, shown_elsewhere: bool, confirm_enabled: bool) -> Option<Prompt> {
    let session = session?;
    if !confirm_enabled || shown_elsewhere || !has_running_process(session) {
        return None;
    }
    let title = match scope {
        Scope::Pane | Scope::Last => "Close this pane?",
        Scope::Tab => "Close this tab?",
    };
    Some(Prompt {
        title: title.to_string(),
        close_title: format!("Close {}", scope.noun()),
        kill_title: "End Session".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_follows_panes_then_tabs_and_never_names_the_window() {
        assert_eq!(Scope::of(2, 1), Scope::Pane);
        assert_eq!(Scope::of(1, 3), Scope::Tab);
        assert_eq!(Scope::of(1, 1), Scope::Last);
        assert_eq!(Scope::Last.noun(), "Pane");
    }

    #[test]
    fn only_something_worth_losing_asks() {
        let agent = Session::new("a", "claude", SessionState::Idle);
        let shell_idle = Session::new("b", "shell", SessionState::Idle);
        let shell_busy = Session::new("c", "shell", SessionState::Working);
        let done = Session::new("d", "claude", SessionState::Done);
        assert!(decide(None, Scope::Pane, false, true).is_none(), "empty pane");
        assert!(decide(Some(&shell_idle), Scope::Pane, false, true).is_none(), "shell at a prompt");
        assert!(decide(Some(&done), Scope::Pane, false, true).is_none(), "finished");
        assert!(decide(Some(&agent), Scope::Pane, true, true).is_none(), "shown elsewhere");
        assert!(decide(Some(&agent), Scope::Pane, false, false).is_none(), "confirmation off");
        let p = decide(Some(&shell_busy), Scope::Tab, false, true).unwrap_or_else(|| unreachable!());
        assert_eq!(
            (p.title.as_str(), p.close_title.as_str(), p.kill_title.as_str()),
            ("Close this tab?", "Close Tab", "End Session")
        );
        let p = decide(Some(&agent), Scope::Last, false, true).unwrap_or_else(|| unreachable!());
        assert_eq!((p.title.as_str(), p.close_title.as_str()), ("Close this pane?", "Close Pane"));
    }
}
