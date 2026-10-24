//! A shell session's live name (SMOODEV-4127).
//!
//! A shell tab is named the way cmux and Ghostty name one: by the program
//! running in it, else by the directory it sits in. cmux gets that from the
//! terminal title its shell integration sets; SmoothFlow's shells run under
//! tmux on the engine side, so the engine reads the same facts off the pane
//! (`#{pane_current_command}`, `#{pane_current_path}`, `#{pane_title}`) on
//! its supervision tick and writes the result to the row's `title` — which
//! is how the Mac app, the phones and `th flow ls` all see one name. A title
//! someone gave the session (`flow.rename`, `title_pinned`) is never
//! overwritten.
//!
//! The rules, pure:
//!
//! 1. At the prompt (the foreground process is a shell): the directory —
//!    its folder name, `~` for home, `/` for the root.
//! 2. A program is running: the title it set (OSC 0/2), cleaned up, when it
//!    set one *since the prompt*; else its command name (`cargo`, `nvim`).
//!
//! tmux keeps a pane's title after the program that set it exits, so a
//! title is only believed when it differs from the one the pane had at the
//! prompt (`baseline`): `vim` titling the pane `notes.md` and exiting must
//! not name the next `cargo build` `notes.md`.

use std::collections::HashMap;
use std::path::Path;

/// What one read of a pane says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneFacts {
    /// `#{pane_current_command}` — the foreground process's name.
    pub command: String,
    /// `#{pane_current_path}`, when tmux could read it.
    pub path: Option<String>,
    /// `#{pane_title}` — OSC 0/2, or tmux's default (the host name).
    pub title: String,
    /// `#{host}` / `#{host_short}`: a title equal to either is tmux's
    /// default, not something a program said.
    pub hosts: Vec<String>,
}

/// The longest name a tab gets; longer ones end in `…`.
pub const MAX_LEN: usize = 40;

/// Processes that mean "at the prompt". A login shell can show as `-zsh`.
#[must_use]
pub fn is_shell(command: &str) -> bool {
    let c = command.trim().trim_start_matches('-');
    let c = c.rsplit('/').next().unwrap_or(c);
    matches!(
        c,
        "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "elvish" | "xonsh" | "login"
    )
}

/// A directory's name in a tab: `~` for home, `/` for the root, else its
/// last component. `None` for a blank path.
#[must_use]
pub fn dir_name(path: &str, home: &str) -> Option<String> {
    let p = path.trim();
    if p.is_empty() {
        return None;
    }
    let trimmed = p.trim_end_matches('/');
    if p == "~" || (!home.is_empty() && trimmed == home.trim_end_matches('/')) {
        return Some("~".into());
    }
    if trimmed.is_empty() {
        return Some("/".into());
    }
    Some(trimmed.rsplit('/').next().unwrap_or(trimmed).to_string())
}

/// The title a program set, cleaned up for a tab — or `None` when the pane
/// has no title of its own (blank, or tmux's host-name default).
///
/// A shell theme's `user@host: ~/dir` loses its `user@host:` prefix, and a
/// title that is just a path becomes its folder name.
#[must_use]
pub fn program_title(title: &str, hosts: &[String], home: &str) -> Option<String> {
    let t = title.trim();
    if t.is_empty() || hosts.iter().any(|h| !h.is_empty() && h.eq_ignore_ascii_case(t)) {
        return None;
    }
    let t = strip_user_host(t);
    let t = if t.starts_with('/') || t.starts_with('~') {
        dir_name(t, home)?
    } else {
        t.to_string()
    };
    (!t.is_empty()).then(|| clip(&t))
}

/// `user@host: rest` / `user@host:rest` → `rest`; anything else unchanged.
fn strip_user_host(t: &str) -> &str {
    let Some((head, rest)) = t.split_once(':') else { return t };
    if head.contains('@') && !head.contains(char::is_whitespace) {
        let rest = rest.trim();
        if !rest.is_empty() {
            return rest;
        }
    }
    t
}

/// At most [`MAX_LEN`] characters, `…` marking a cut.
#[must_use]
pub fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_LEN {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_LEN - 1).collect();
    out.push('…');
    out
}

/// One pane's name from one read, given the title it had at the prompt.
#[must_use]
pub fn shell_title(facts: &PaneFacts, baseline: Option<&str>, home: &str) -> Option<String> {
    if facts.command.trim().is_empty() || is_shell(&facts.command) {
        return facts.path.as_deref().and_then(|p| dir_name(p, home));
    }
    let fresh = baseline.is_none_or(|b| b != facts.title);
    if fresh {
        if let Some(t) = program_title(&facts.title, &facts.hosts, home) {
            return Some(t);
        }
    }
    let cmd = facts.command.trim();
    let cmd = cmd.rsplit('/').next().unwrap_or(cmd);
    Some(clip(cmd))
}

/// The per-session memory [`shell_title`] needs: each pane's title as of its
/// last prompt.
#[derive(Debug, Default)]
pub struct Titler {
    baselines: HashMap<String, String>,
}

impl Titler {
    /// Name session `id` from this read, remembering its prompt-time title.
    pub fn name(&mut self, id: &str, facts: &PaneFacts, home: &str) -> Option<String> {
        if facts.command.trim().is_empty() || is_shell(&facts.command) {
            self.baselines.insert(id.to_string(), facts.title.clone());
        }
        shell_title(facts, self.baselines.get(id).map(String::as_str), home)
    }

    /// Forget a session (it ended).
    pub fn forget(&mut self, id: &str) {
        self.baselines.remove(id);
    }
}

/// A new shell's first name, before any read: its directory's.
#[must_use]
pub fn initial(dir: &Path, home: &str) -> String {
    dir_name(&dir.to_string_lossy(), home).unwrap_or_else(|| "shell".into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    const HOME: &str = "/Users/me";

    fn facts(command: &str, path: &str, title: &str) -> PaneFacts {
        PaneFacts {
            command: command.into(),
            path: Some(path.into()),
            title: title.into(),
            hosts: vec!["mac.local".into(), "mac".into()],
        }
    }

    #[test]
    fn shells_are_recognised_whatever_the_spelling() {
        for c in ["zsh", "-zsh", "/bin/bash", "fish", "sh", "nu"] {
            assert!(is_shell(c), "{c}");
        }
        for c in ["cargo", "nvim", "node", "zshdb", ""] {
            assert!(!is_shell(c), "{c}");
        }
    }

    #[test]
    fn dir_names() {
        assert_eq!(dir_name("/Users/me", HOME).as_deref(), Some("~"));
        assert_eq!(dir_name("/Users/me/", HOME).as_deref(), Some("~"));
        assert_eq!(dir_name("~", HOME).as_deref(), Some("~"));
        assert_eq!(dir_name("/", HOME).as_deref(), Some("/"));
        assert_eq!(dir_name("/Users/me/dev/smooth/", HOME).as_deref(), Some("smooth"));
        assert_eq!(dir_name("  ", HOME), None);
    }

    #[test]
    fn at_the_prompt_the_tab_is_the_directory() {
        assert_eq!(
            shell_title(&facts("zsh", "/Users/me/dev/smooth", "mac.local"), None, HOME).as_deref(),
            Some("smooth")
        );
        assert_eq!(shell_title(&facts("-zsh", "/Users/me", "mac"), None, HOME).as_deref(), Some("~"));
        // A shell theme's title is ignored at the prompt: the cwd is the name.
        assert_eq!(shell_title(&facts("zsh", "/tmp", "me@mac: /tmp"), None, HOME).as_deref(), Some("tmp"));
        // No path read: no name (the caller keeps the old one).
        let mut f = facts("zsh", "", "");
        f.path = None;
        assert_eq!(shell_title(&f, None, HOME), None);
    }

    #[test]
    fn a_running_program_names_the_tab() {
        assert_eq!(
            shell_title(&facts("cargo", "/Users/me/dev/x", "mac.local"), Some("mac.local"), HOME).as_deref(),
            Some("cargo")
        );
        // …by the title it set, when it set one.
        assert_eq!(
            shell_title(&facts("nvim", "/Users/me/dev/x", "notes.md - NVIM"), Some("mac.local"), HOME).as_deref(),
            Some("notes.md - NVIM")
        );
        assert_eq!(shell_title(&facts("/usr/bin/python3", "/", ""), None, HOME).as_deref(), Some("python3"));
    }

    #[test]
    fn a_stale_title_from_an_earlier_program_is_not_believed() {
        // vim titled the pane and exited; the prompt saw that title; now cargo runs.
        let f = facts("cargo", "/Users/me/dev/x", "notes.md - VIM");
        assert_eq!(shell_title(&f, Some("notes.md - VIM"), HOME).as_deref(), Some("cargo"));
    }

    #[test]
    fn program_titles_are_cleaned() {
        let hosts = vec!["mac.local".to_string()];
        assert_eq!(program_title("me@mac: ~/dev/smooth", &hosts, HOME).as_deref(), Some("smooth"));
        assert_eq!(program_title("me@mac:/Users/me", &hosts, HOME).as_deref(), Some("~"));
        assert_eq!(program_title("MAC.LOCAL", &hosts, HOME), None, "tmux's default title");
        assert_eq!(program_title("   ", &hosts, HOME), None);
        assert_eq!(program_title("✳ Fix the parser", &hosts, HOME).as_deref(), Some("✳ Fix the parser"));
        assert_eq!(program_title("a b: c", &hosts, HOME).as_deref(), Some("a b: c"), "not a user@host prefix");
        let long = "x".repeat(80);
        let clipped = program_title(&long, &hosts, HOME).unwrap();
        assert_eq!(clipped.chars().count(), MAX_LEN);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn titler_tracks_the_prompt_time_title_per_session() {
        let mut t = Titler::default();
        // At the prompt, a theme set a title: that becomes the baseline.
        assert_eq!(t.name("a", &facts("zsh", "/Users/me/x", "me@mac: ~/x"), HOME).as_deref(), Some("x"));
        // `make` runs and sets nothing: the stale theme title is not its name.
        assert_eq!(t.name("a", &facts("make", "/Users/me/x", "me@mac: ~/x"), HOME).as_deref(), Some("make"));
        // `htop` sets its own.
        assert_eq!(t.name("a", &facts("htop", "/Users/me/x", "htop"), HOME).as_deref(), Some("htop"));
        // Another session has its own baseline.
        assert_eq!(t.name("b", &facts("make", "/Users/me/x", "me@mac: ~/x"), HOME).as_deref(), Some("x"));
        t.forget("a");
        assert_eq!(t.name("a", &facts("make", "/Users/me/x", "me@mac: ~/x"), HOME).as_deref(), Some("x"));
    }

    #[test]
    fn initial_name_is_the_directory() {
        assert_eq!(initial(Path::new("/Users/me/dev/smooth"), HOME), "smooth");
        assert_eq!(initial(Path::new("/Users/me"), HOME), "~");
        assert_eq!(initial(Path::new(""), HOME), "shell");
    }
}
