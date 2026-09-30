//! The New Session sheet's state and keyboard (spec §6), toolkit-free.
//!
//! The view draws it and runs the [`Effect`]s it asks for (HTTP searches,
//! the `flow.new` frame). Rules — picker rows, path expansion, arrow
//! clamping — come from `smooth-flow-client`.

use smooth_flow_client::directory;
use smooth_flow_client::harness::{self, Harness, PickerRow};

use crate::field::{Edit, Field};
use crate::frames::NewSession;
use crate::http::{Inferred, Repo, RepoList};

/// Which control has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Kind,
    Directory,
    Prompt,
}

/// Something the view must do for the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// `GET /api/flow/repos?q=`; hand the answer to [`Sheet::repos_loaded`] with `generation`.
    Search { query: String, generation: u64 },
    /// `GET /api/flow/infer?cwd=`; hand the answer to [`Sheet::inferred_loaded`].
    Infer { cwd: Option<String>, generation: u64 },
    /// Send `flow.new` and close the sheet.
    Start(NewSession),
    /// Close the sheet.
    Cancel,
}

#[derive(Debug, Clone)]
pub struct Sheet {
    pub rows: Vec<PickerRow>,
    pub kind: usize,
    pub focus: Focus,
    pub directory: Field,
    pub prompt: Field,
    pub matches: Vec<Repo>,
    pub highlighted: usize,
    pub scanning: bool,
    /// The directory picked (a match or a typed path); `None` = the inferred one.
    pub chosen: Option<String>,
    pub inferred: Option<Inferred>,
    /// The `cwd` the current inference was asked for.
    inferred_for: Option<String>,
    pub error: Option<String>,
    search_gen: u64,
    infer_gen: u64,
    home: String,
}

impl Sheet {
    /// A fresh sheet, plus what to load first: inference for `seed_cwd` (the
    /// focused session's worktree, or the daemon's workspace) and the recent
    /// repos.
    #[must_use]
    pub fn open(harnesses: &[Harness], home: &str, seed_cwd: Option<&str>) -> (Self, Vec<Effect>) {
        let rows = harness::picker(harnesses, true);
        let default = harness::default_kind(&rows);
        let kind = rows.iter().position(|r| Some(&r.kind) == default.as_ref()).unwrap_or(0);
        let mut s = Self {
            rows,
            kind,
            focus: Focus::Kind,
            directory: Field::default(),
            prompt: Field::default(),
            matches: Vec::new(),
            highlighted: 0,
            scanning: false,
            chosen: None,
            inferred: None,
            inferred_for: None,
            error: None,
            search_gen: 0,
            infer_gen: 0,
            home: home.to_string(),
        };
        let effects = vec![s.infer(seed_cwd.map(str::to_string)), s.search()];
        (s, effects)
    }

    /// `flow.harnesses` replaced the list: keep the selection by kind.
    pub fn set_harnesses(&mut self, harnesses: &[Harness]) {
        let current = self.selected().map(|r| r.kind.clone());
        self.rows = harness::picker(harnesses, true);
        self.kind = current
            .and_then(|k| self.rows.iter().position(|r| r.kind == k))
            .or_else(|| {
                let d = harness::default_kind(&self.rows);
                self.rows.iter().position(|r| Some(&r.kind) == d.as_ref())
            })
            .unwrap_or(0);
    }

    #[must_use]
    pub fn selected(&self) -> Option<&PickerRow> {
        self.rows.get(self.kind)
    }

    /// Start is blocked only by a harness that isn't installed — never by a
    /// missing pearl (spec §6).
    #[must_use]
    pub fn can_start(&self) -> bool {
        self.selected().is_some_and(|r| r.enabled)
    }

    fn infer(&mut self, cwd: Option<String>) -> Effect {
        self.infer_gen += 1;
        self.inferred_for.clone_from(&cwd);
        Effect::Infer {
            cwd,
            generation: self.infer_gen,
        }
    }

    fn search(&mut self) -> Effect {
        self.search_gen += 1;
        Effect::Search {
            query: self.directory.text().trim().to_string(),
            generation: self.search_gen,
        }
    }

    /// A search answered; stale answers are dropped.
    pub fn repos_loaded(&mut self, generation: u64, result: Result<RepoList, String>) {
        if generation != self.search_gen {
            return;
        }
        match result {
            Ok(list) => {
                self.matches = list.repos;
                self.scanning = list.scanning;
                self.highlighted = directory::moved(self.highlighted, 0, self.matches.len());
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Inference answered; stale answers are dropped.
    pub fn inferred_loaded(&mut self, generation: u64, result: Result<Inferred, String>) {
        if generation != self.infer_gen {
            return;
        }
        match result {
            Ok(i) => self.inferred = Some(i),
            Err(e) => self.error = Some(e),
        }
    }

    /// Use `path` as the directory; picking re-runs inference so the pearl,
    /// branch and title follow (spec §6).
    pub fn pick(&mut self, path: &str) -> Effect {
        self.chosen = Some(path.to_string());
        self.directory.set(&directory::abbreviate(path, &self.home));
        self.matches.clear();
        self.inferred = None;
        self.infer(Some(path.to_string()))
    }

    /// The directory the session will run in, when known.
    #[must_use]
    pub fn effective_directory(&self) -> Option<String> {
        self.chosen
            .clone()
            .or_else(|| directory::expanded_path(self.directory.text(), &self.home))
            .or_else(|| self.inferred.as_ref().map(|i| i.worktree.clone()).filter(|w| !w.is_empty()))
    }

    /// The `flow.new` this sheet would send now.
    #[must_use]
    pub fn new_session(&self) -> Option<NewSession> {
        let row = self.selected().filter(|r| r.enabled)?;
        let dir = self.effective_directory();
        // The inferred pearl and project describe the directory they were
        // inferred for; after a different path is typed, leave them to the engine.
        let from_inference = self.chosen.is_none() && directory::expanded_path(self.directory.text(), &self.home).is_none();
        let inferred = self.inferred.as_ref().filter(|_| from_inference || self.inferred_for == dir);
        Some(NewSession {
            kind: row.kind.clone(),
            worktree: dir,
            project: inferred.map(|i| i.project.clone()).filter(|p| !p.is_empty()),
            pearl_id: inferred.and_then(|i| i.pearl_id.clone()),
            prompt: Some(self.prompt.text().to_string()).filter(|p| !p.trim().is_empty()),
            title: None,
        })
    }

    fn start(&self) -> Vec<Effect> {
        self.new_session().map(Effect::Start).into_iter().collect()
    }

    /// Next / previous control (Tab / Shift+Tab).
    pub fn cycle_focus(&mut self, back: bool) {
        self.focus = match (self.focus, back) {
            (Focus::Kind, false) | (Focus::Prompt, true) => Focus::Directory,
            (Focus::Directory, false) | (Focus::Kind, true) => Focus::Prompt,
            (Focus::Prompt, false) | (Focus::Directory, true) => Focus::Kind,
        };
    }

    /// A keystroke while the sheet is up (GPUI key name, typed text,
    /// Ctrl/Super held, Shift held).
    pub fn key(&mut self, key: &str, key_char: Option<&str>, command: bool, shift: bool) -> Vec<Effect> {
        if key == "tab" {
            self.cycle_focus(shift);
            return Vec::new();
        }
        match self.focus {
            Focus::Kind => match key {
                "up" | "left" => {
                    self.kind = directory::moved(self.kind, -1, self.rows.len());
                    Vec::new()
                }
                "down" | "right" => {
                    self.kind = directory::moved(self.kind, 1, self.rows.len());
                    Vec::new()
                }
                "enter" => self.start(),
                "escape" => vec![Effect::Cancel],
                _ => Vec::new(),
            },
            Focus::Directory => self.directory_key(key, key_char, command),
            Focus::Prompt => match self.prompt.key(key, key_char, command) {
                Edit::Ignored if key == "enter" => self.start(),
                Edit::Ignored if key == "escape" => vec![Effect::Cancel],
                _ => Vec::new(),
            },
        }
    }

    fn directory_key(&mut self, key: &str, key_char: Option<&str>, command: bool) -> Vec<Effect> {
        match key {
            "up" => {
                self.highlighted = directory::moved(self.highlighted, -1, self.matches.len());
                return Vec::new();
            }
            "down" => {
                self.highlighted = directory::moved(self.highlighted, 1, self.matches.len());
                return Vec::new();
            }
            "escape" => {
                // Esc clears the field; on an empty field it cancels.
                if self.directory.text().is_empty() && self.chosen.is_none() {
                    return vec![Effect::Cancel];
                }
                self.directory.set("");
                self.chosen = None;
                self.highlighted = 0;
                return vec![self.search(), self.infer(None)];
            }
            "enter" => {
                // A typed path is used as-is; otherwise Return picks the
                // highlighted match; with nothing to pick, it starts.
                if self.chosen.is_none() {
                    if let Some(p) = directory::expanded_path(self.directory.text(), &self.home) {
                        return vec![self.pick(&p)];
                    }
                    if let Some(r) = self.matches.get(self.highlighted).map(|r| r.path.clone()) {
                        return vec![self.pick(&r)];
                    }
                }
                return self.start();
            }
            _ => {}
        }
        match self.directory.key(key, key_char, command) {
            Edit::Changed => {
                self.chosen = None;
                self.highlighted = 0;
                if directory::expanded_path(self.directory.text(), &self.home).is_some() {
                    self.matches.clear();
                    Vec::new()
                } else {
                    vec![self.search()]
                }
            }
            Edit::Moved | Edit::Ignored => Vec::new(),
        }
    }

    /// The inferred-context line (spec §6): the title, then pearl · Jira ·
    /// branch · worktree, then a note.
    #[must_use]
    pub fn context_lines(&self) -> Option<(String, String, Option<&'static str>)> {
        let i = self.inferred.as_ref()?;
        let facts: Vec<String> = [
            i.pearl_id.clone(),
            i.jira_key.clone(),
            i.branch.clone(),
            Some(directory::abbreviate(&i.worktree, &self.home)).filter(|w| !w.is_empty()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let note = if !i.is_git {
            Some("not a git worktree — no pearl, no branch")
        } else if i.pearl_id.is_none() {
            Some("no pearl here — starting anyway is fine")
        } else {
            None
        };
        Some((i.title.clone(), facts.join(" · "), note))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harnesses() -> Vec<Harness> {
        let mut codex = Harness::new("codex");
        codex.installed = false;
        vec![codex, Harness::new("claude")]
    }

    fn inferred(worktree: &str, pearl: Option<&str>) -> Inferred {
        Inferred {
            worktree: worktree.into(),
            project: worktree.into(),
            is_git: true,
            pearl_id: pearl.map(Into::into),
            title: "t".into(),
            ..Inferred::default()
        }
    }

    #[test]
    fn opens_on_the_first_startable_kind_and_loads_context() {
        let (s, fx) = Sheet::open(&harnesses(), "/home/me", Some("/w/x"));
        assert_eq!(s.selected().map(|r| r.kind.as_str()), Some("claude"));
        assert_eq!(
            fx,
            vec![
                Effect::Infer {
                    cwd: Some("/w/x".into()),
                    generation: 1
                },
                Effect::Search {
                    query: String::new(),
                    generation: 1
                }
            ]
        );
        let mut s = s;
        s.key("up", None, false, false);
        assert_eq!(s.selected().map(|r| r.kind.as_str()), Some("codex"));
        assert!(!s.can_start(), "a missing harness can't start");
        assert!(s.key("enter", None, false, false).is_empty());
        s.set_harnesses(&[Harness::new("claude")]);
        assert_eq!(
            s.selected().map(|r| r.kind.as_str()),
            Some("claude"),
            "the vanished selection falls back to the default"
        );
    }

    #[test]
    fn typing_searches_arrows_move_return_picks_and_reinfers() {
        let (mut s, _) = Sheet::open(&harnesses(), "/home/me", None);
        s.key("tab", None, false, false);
        assert_eq!(s.focus, Focus::Directory);
        let fx = s.key("s", Some("s"), false, false);
        assert_eq!(
            fx,
            vec![Effect::Search {
                query: "s".into(),
                generation: 2
            }]
        );
        s.repos_loaded(1, Ok(RepoList::default()));
        assert!(s.matches.is_empty(), "a stale answer is dropped");
        let repo = |p: &str| Repo {
            path: p.into(),
            name: String::new(),
            branch: None,
            main: None,
        };
        s.repos_loaded(
            2,
            Ok(RepoList {
                repos: vec![repo("/w/a"), repo("/w/b")],
                scanning: false,
                indexed: true,
            }),
        );
        s.key("down", None, false, false);
        s.key("down", None, false, false);
        assert_eq!(s.highlighted, 1, "clamped");
        let fx = s.key("enter", None, false, false);
        assert_eq!(
            fx,
            vec![Effect::Infer {
                cwd: Some("/w/b".into()),
                generation: 2
            }]
        );
        assert_eq!(s.directory.text(), "/w/b");
        s.inferred_loaded(2, Ok(inferred("/w/b", Some("th-1"))));
        let Some(Effect::Start(n)) = s.key("enter", None, false, false).pop() else {
            panic!("starts")
        };
        assert_eq!(
            (n.kind.as_str(), n.worktree.as_deref(), n.pearl_id.as_deref()),
            ("claude", Some("/w/b"), Some("th-1"))
        );
    }

    #[test]
    fn a_typed_path_is_used_as_is_and_esc_clears_then_cancels() {
        let (mut s, _) = Sheet::open(&harnesses(), "/home/me", None);
        s.inferred_loaded(1, Ok(inferred("/home/me", None)));
        s.focus = Focus::Directory;
        for c in ["~", "/", "d"] {
            s.key(c, Some(c), false, false);
        }
        assert!(s.matches.is_empty());
        assert_eq!(s.effective_directory().as_deref(), Some("/home/me/d"));
        let n = s.new_session().unwrap_or_default();
        assert_eq!(
            (n.worktree.as_deref(), n.project),
            (Some("/home/me/d"), None),
            "the other directory's project isn't guessed"
        );
        assert_eq!(
            s.key("enter", None, false, false),
            vec![Effect::Infer {
                cwd: Some("/home/me/d".into()),
                generation: 2
            }]
        );
        assert_eq!(s.key("escape", None, false, false).len(), 2, "clears: search again, infer the default again");
        assert_eq!(s.directory.text(), "");
        assert_eq!(s.key("escape", None, false, false), vec![Effect::Cancel]);
    }

    #[test]
    fn with_nothing_picked_start_uses_the_inferred_context_and_the_prompt() {
        let (mut s, _) = Sheet::open(&harnesses(), "/home/me", Some("/w/x"));
        s.inferred_loaded(1, Ok(inferred("/w/x", Some("th-9"))));
        s.focus = Focus::Prompt;
        for c in ["h", "i"] {
            s.key(c, Some(c), false, false);
        }
        let Some(Effect::Start(n)) = s.key("enter", None, false, false).pop() else {
            panic!("starts")
        };
        assert_eq!(
            (n.worktree.as_deref(), n.project.as_deref(), n.pearl_id.as_deref(), n.prompt.as_deref()),
            (Some("/w/x"), Some("/w/x"), Some("th-9"), Some("hi"))
        );
        let (title, facts, note) = s.context_lines().unwrap_or_default();
        assert_eq!((title.as_str(), facts.as_str(), note), ("t", "th-9 · /w/x", None));
        s.inferred = Some(Inferred {
            is_git: false,
            ..inferred("/tmp", None)
        });
        assert_eq!(s.context_lines().and_then(|c| c.2), Some("not a git worktree — no pearl, no branch"));
    }
}
