//! Which center tabs a session can use (spec §4). Diff and PR follow the
//! worktree; only Activity follows the harness. Tabs are disabled, never hidden.

use serde::{Deserialize, Serialize};

/// The center tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CenterTab {
    Terminal,
    Diff,
    Pr,
    Activity,
}

/// How much the Activity tab has to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityDepth {
    /// `hooks` / `native` harnesses: the full event log.
    Full,
    /// `scrape` harnesses: supervision events only.
    Thin,
    /// A shell reports nothing.
    None,
}

/// The gate for the focused session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub diff: bool,
    pub pr: bool,
    pub activity: ActivityDepth,
}

impl Gate {
    /// No session focused: terminal only.
    pub const NOTHING: Self = Self {
        diff: false,
        pr: false,
        activity: ActivityDepth::None,
    };

    /// The gate from the session's kind and branch, the harness's
    /// `state_source`, and — once loaded — the handoff packet's `head` and
    /// `branch`. Before the packet arrives a branch on the row counts as "in a
    /// repo", so Diff doesn't start greyed out and flicker on.
    #[must_use]
    pub fn of(kind: &str, session_branch: Option<&str>, harness_state_source: Option<&str>, packet: Option<(Option<&str>, Option<&str>)>) -> Self {
        let branch = usable_branch(packet.and_then(|(_, b)| b).or(session_branch));
        let in_repo = packet.map_or_else(|| branch.is_some(), |(head, _)| head.is_some_and(|h| !h.trim().is_empty()));
        Self {
            diff: in_repo,
            pr: in_repo && branch.is_some(),
            activity: activity_depth(kind, harness_state_source),
        }
    }

    #[must_use]
    pub const fn allows(self, tab: CenterTab) -> bool {
        match tab {
            CenterTab::Terminal => true,
            CenterTab::Diff => self.diff,
            CenterTab::Pr => self.pr,
            CenterTab::Activity => !matches!(self.activity, ActivityDepth::None),
        }
    }

    /// The tab to land on when the current one isn't allowed.
    #[must_use]
    pub const fn resolve(self, tab: CenterTab) -> CenterTab {
        if self.allows(tab) {
            tab
        } else {
            CenterTab::Terminal
        }
    }

    /// The tooltip for a disabled tab, so it never reads as broken.
    #[must_use]
    pub const fn why_not(self, tab: CenterTab) -> Option<&'static str> {
        if self.allows(tab) {
            return None;
        }
        match tab {
            CenterTab::Terminal => None,
            CenterTab::Diff => Some("Not in a git repository"),
            CenterTab::Pr => Some(if self.diff { "No branch (detached HEAD)" } else { "Not in a git repository" }),
            CenterTab::Activity => Some("A shell reports no activity"),
        }
    }
}

/// `HEAD` (detached) or blank is no branch to hang a PR on.
#[must_use]
pub fn usable_branch(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|b| !b.is_empty() && *b != "HEAD")
}

/// Activity depth from the kind and the harness's `state_source`. An unknown
/// harness keeps the full view rather than hiding events it may well send.
#[must_use]
pub fn activity_depth(kind: &str, harness_state_source: Option<&str>) -> ActivityDepth {
    if kind == "shell" {
        return ActivityDepth::None;
    }
    if harness_state_source == Some("scrape") {
        ActivityDepth::Thin
    } else {
        ActivityDepth::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_and_pr_follow_the_worktree_activity_follows_the_kind() {
        let shell_in_repo = Gate::of("shell", Some("main"), None, None);
        assert!(shell_in_repo.diff && shell_in_repo.pr, "a shell in a worktree has a diff and a PR tab");
        assert_eq!(shell_in_repo.activity, ActivityDepth::None);
        assert_eq!(shell_in_repo.why_not(CenterTab::Activity), Some("A shell reports no activity"));

        let detached = Gate::of("claude", None, Some("hooks"), Some((Some("abc123"), Some("HEAD"))));
        assert!(detached.diff && !detached.pr);
        assert_eq!(detached.why_not(CenterTab::Pr), Some("No branch (detached HEAD)"));

        let no_repo = Gate::of("aider", Some("main"), Some("scrape"), Some((None, None)));
        assert!(!no_repo.diff, "the packet's missing HEAD beats the row's branch");
        assert_eq!(no_repo.activity, ActivityDepth::Thin);
        assert_eq!(no_repo.resolve(CenterTab::Diff), CenterTab::Terminal);
        assert!(Gate::NOTHING.allows(CenterTab::Terminal));
    }
}
