//! What a tab or row calls a session (spec §5).

use crate::session::Session;

/// A title that is only a path — inference titles a session by its directory.
#[must_use]
pub fn looks_like_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('~')
}

/// `/a/b/c` → `c`, `~` or `home` → `~`, `/` → `/`; `None` for a blank path.
/// A trailing slash is ignored.
#[must_use]
pub fn folder_name(path: &str, home: &str) -> Option<String> {
    let p = path.trim();
    if p.is_empty() {
        return None;
    }
    let trimmed = p.trim_end_matches('/');
    if p == "~" || (!home.is_empty() && trimmed == home.trim_end_matches('/')) {
        return Some("~".to_string());
    }
    if trimmed.is_empty() {
        return Some("/".to_string());
    }
    Some(trimmed.rsplit('/').next().unwrap_or(trimmed).to_string())
}

/// The tab title: the pearl id, else a title that isn't just a path, else the
/// folder name (of the path title, or of the worktree, or the project), else
/// the kind. Never a whole path.
#[must_use]
pub fn tab_title(s: &Session, home: &str) -> String {
    if let Some(p) = s.pearl_id.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        return p.to_string();
    }
    let t = s.title.trim();
    if !t.is_empty() && !looks_like_path(t) {
        return t.to_string();
    }
    let dir = if !t.is_empty() {
        t
    } else if !s.worktree.is_empty() {
        &s.worktree
    } else {
        &s.project
    };
    folder_name(dir, home).unwrap_or_else(|| s.kind.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionState;

    fn s(title: &str, pearl: Option<&str>, worktree: &str) -> Session {
        Session {
            title: title.into(),
            pearl_id: pearl.map(Into::into),
            worktree: worktree.into(),
            ..Session::new("x", "claude", SessionState::Idle)
        }
    }

    #[test]
    fn a_tab_is_never_a_whole_path() {
        assert_eq!(tab_title(&s("fix the parser", Some("th-1"), ""), "/Users/me"), "th-1");
        assert_eq!(tab_title(&s("fix the parser", Some("  "), ""), "/Users/me"), "fix the parser");
        assert_eq!(tab_title(&s("~/dev/smooai/smooai", None, ""), "/Users/me"), "smooai");
        assert_eq!(tab_title(&s("/", None, ""), "/Users/me"), "/");
        assert_eq!(tab_title(&s("", None, "/Users/me/dev/x/"), "/Users/me"), "x");
        assert_eq!(tab_title(&s("", None, "/Users/me"), "/Users/me"), "~");
        assert_eq!(tab_title(&s("", None, ""), "/Users/me"), "claude", "nothing else: the kind");
    }
}
