//! SmoothFlow Diff client rules (spec §14, th-26f5b9): the pure decisions
//! every client's native diff viewer makes over the engine's `flow.diff`
//! payload — side-by-side row pairing, the file tree, keyboard navigation
//! order, what starts collapsed, the default base, and the viewer's keys.
//!
//! The engine computes the diff (hunks, word spans, syntax spans); none of
//! this re-diffs anything.

use serde::{Deserialize, Serialize};

/// A diff base (the wire's spelling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Base {
    Turn,
    Uncommitted,
    Branch,
}

/// A line's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Add,
    Del,
    Ctx,
}

/// The slice of a wire line these rules read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub kind: LineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<u32>,
    #[serde(default)]
    pub text: String,
}

/// The slice of a wire hunk these rules read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub id: String,
    #[serde(default)]
    pub lines: Vec<Line>,
}

/// The slice of a wire file these rules read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: String,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub deleted: u32,
    #[serde(default)]
    pub binary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noise: Option<String>,
    #[serde(default)]
    pub collapsed_by_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunks_omitted: Option<String>,
    #[serde(default)]
    pub hunks: Vec<Hunk>,
}

// ── side by side ─────────────────────────────────────────────────────────────

/// One side-by-side row: indexes into the hunk's `lines` for the left (old)
/// and right (new) columns; `None` is a blank cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub left: Option<usize>,
    pub right: Option<usize>,
}

/// Pair a hunk's lines into side-by-side rows. Context fills both columns;
/// in a change block (a run of `del`s then a run of `add`s) the i-th del
/// sits beside the i-th add, and the longer run's rest gets blank cells on
/// the other side. An `add` run with no `del`s before it is all right-side.
#[must_use]
pub fn side_by_side(lines: &[Line]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            LineKind::Ctx => {
                rows.push(Row { left: Some(i), right: Some(i) });
                i += 1;
            }
            LineKind::Del | LineKind::Add => {
                let del_start = i;
                while i < lines.len() && lines[i].kind == LineKind::Del {
                    i += 1;
                }
                let add_start = i;
                while i < lines.len() && lines[i].kind == LineKind::Add {
                    i += 1;
                }
                let (dels, adds) = (add_start - del_start, i - add_start);
                for k in 0..dels.max(adds) {
                    rows.push(Row {
                        left: (k < dels).then_some(del_start + k),
                        right: (k < adds).then_some(add_start + k),
                    });
                }
            }
        }
    }
    rows
}

// ── file tree ────────────────────────────────────────────────────────────────

/// One row of the file tree sidebar, already flattened in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeRow {
    /// `dir` or `file`.
    pub kind: String,
    /// What the row shows: a file's name, or a directory chain compressed
    /// to `a/b/c` when each level has exactly one child directory and no files.
    pub name: String,
    /// The full path (a directory's has no trailing slash).
    pub path: String,
    pub depth: usize,
    /// For a file row: its index in the diff's `files`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<usize>,
}

#[derive(Default)]
struct Dir {
    name: String,
    dirs: Vec<Dir>,
    files: Vec<(String, usize)>,
}

impl Dir {
    fn insert(&mut self, parts: &[&str], idx: usize) {
        match parts {
            [] => {}
            [file] => self.files.push(((*file).to_string(), idx)),
            [dir, rest @ ..] => {
                let pos = self.dirs.iter().position(|d| d.name == *dir).unwrap_or_else(|| {
                    self.dirs.push(Self {
                        name: (*dir).to_string(),
                        ..Self::default()
                    });
                    self.dirs.len() - 1
                });
                self.dirs[pos].insert(rest, idx);
            }
        }
    }

    fn flatten(&self, prefix: &str, depth: usize, out: &mut Vec<TreeRow>) {
        let key = |s: &str| (s.to_lowercase(), s.to_string());
        let mut dirs: Vec<&Self> = self.dirs.iter().collect();
        dirs.sort_by_key(|d| key(&d.name));
        for d in dirs {
            // Compress single-child directory chains: `src/ui/views`.
            let mut name = d.name.clone();
            let mut node = d;
            while node.files.is_empty() && node.dirs.len() == 1 {
                node = &node.dirs[0];
                name = format!("{name}/{}", node.name);
            }
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            out.push(TreeRow {
                kind: "dir".into(),
                name,
                path: path.clone(),
                depth,
                file: None,
            });
            node.flatten(&path, depth + 1, out);
        }
        let mut files: Vec<&(String, usize)> = self.files.iter().collect();
        files.sort_by_key(|(n, _)| key(n));
        for (n, idx) in files {
            out.push(TreeRow {
                kind: "file".into(),
                name: n.clone(),
                path: if prefix.is_empty() { n.clone() } else { format!("{prefix}/{n}") },
                depth,
                file: Some(*idx),
            });
        }
    }
}

/// The file tree: directories before files at each level, each sorted
/// case-insensitively (ties by exact name), single-child directory chains
/// compressed. Every file appears exactly once.
#[must_use]
pub fn tree(paths: &[&str]) -> Vec<TreeRow> {
    let mut root = Dir::default();
    for (i, p) in paths.iter().enumerate() {
        let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        root.insert(&parts, i);
    }
    let mut out = Vec::new();
    root.flatten("", 0, &mut out);
    out
}

/// File indexes in tree order — the order the main pane lists files in,
/// and the order `]`/`[` walk.
#[must_use]
pub fn file_order(paths: &[&str]) -> Vec<usize> {
    tree(paths).into_iter().filter_map(|r| r.file).collect()
}

// ── collapsed ────────────────────────────────────────────────────────────────

/// How a file starts in the main pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Display {
    pub collapsed: bool,
    /// `viewed` | `noise` | `binary` | `no_content`, when collapsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The hunks aren't in the payload: expanding it requests
    /// `flow.diff{path}` first.
    pub fetch: bool,
}

/// Whether a file starts collapsed, and why. Viewed wins (GitHub's rule:
/// marking a file viewed folds it), then noise, then files with nothing to
/// show inline (binary, a mode-only or pure-rename change).
#[must_use]
pub fn display(f: &File, viewed: bool) -> Display {
    let reason = if viewed {
        Some("viewed")
    } else if f.collapsed_by_default || f.noise.is_some() {
        Some("noise")
    } else if f.binary {
        Some("binary")
    } else if f.hunks.is_empty() && f.hunks_omitted.is_none() {
        Some("no_content")
    } else {
        None
    };
    Display {
        collapsed: reason.is_some(),
        reason: reason.map(str::to_string),
        fetch: f.hunks_omitted.is_some(),
    }
}

/// The key a "viewed" mark is stored under. It changes when the file's
/// change does (the hunk ids, or the counts when hunks aren't loaded), so a
/// file the agent touched again comes back unviewed.
#[must_use]
pub fn viewed_key(f: &File) -> String {
    if f.hunks.is_empty() {
        format!("{}|{}|+{}-{}", f.path, f.status, f.added, f.deleted)
    } else {
        let ids: Vec<&str> = f.hunks.iter().map(|h| h.id.as_str()).collect();
        format!("{}|{}", f.path, ids.join(","))
    }
}

// ── navigation ───────────────────────────────────────────────────────────────

/// A hunk position: `(file index into files, hunk index in that file)`.
pub type Pos = (usize, usize);

/// The next (`forward`) or previous hunk from `at`, walking files in
/// `order` and skipping collapsed files (`collapsed[file]`). From nowhere it
/// lands on the first (forward) or last hunk. Stops at the ends — no wrap.
#[must_use]
pub fn next_hunk(files: &[File], order: &[usize], collapsed: &[bool], at: Option<Pos>, forward: bool) -> Option<Pos> {
    let all: Vec<Pos> = order
        .iter()
        .filter(|&&f| !collapsed.get(f).copied().unwrap_or(false))
        .flat_map(|&f| (0..files.get(f).map_or(0, |x| x.hunks.len())).map(move |h| (f, h)))
        .collect();
    let here = at.and_then(|p| all.iter().position(|q| *q == p));
    match (here, forward) {
        (None, true) => all.first().copied(),
        (None, false) => all.last().copied(),
        (Some(i), true) => all.get(i + 1).copied(),
        (Some(i), false) => i.checked_sub(1).and_then(|j| all.get(j).copied()),
    }
}

/// The next or previous file in `order` (collapsed ones included — `]`/`[`
/// move the selection in the tree). From nowhere: the first or last.
#[must_use]
pub fn next_file(order: &[usize], at: Option<usize>, forward: bool) -> Option<usize> {
    let here = at.and_then(|f| order.iter().position(|x| *x == f));
    match (here, forward) {
        (None, true) => order.first().copied(),
        (None, false) => order.last().copied(),
        (Some(i), true) => order.get(i + 1).copied(),
        (Some(i), false) => i.checked_sub(1).and_then(|j| order.get(j).copied()),
    }
}

// ── base ─────────────────────────────────────────────────────────────────────

/// The base a session's Diff tab opens on: what the last turn changed for
/// an agent, the uncommitted work for a shell.
#[must_use]
pub fn default_base(kind: &str) -> Base {
    if kind == "shell" {
        Base::Uncommitted
    } else {
        Base::Turn
    }
}

/// The picker's label. `branch_ref` is the ref the engine diffed against
/// (from `from.label`, e.g. `origin/main`), shown without its remote.
#[must_use]
pub fn base_label(base: Base, branch_ref: Option<&str>) -> String {
    match base {
        Base::Turn => "Last turn".into(),
        Base::Uncommitted => "Uncommitted".into(),
        Base::Branch => branch_ref
            .map(|r| r.rsplit('/').next().unwrap_or(r))
            .filter(|r| !r.is_empty() && *r != "HEAD")
            .map_or_else(|| "vs default branch".to_string(), |r| format!("vs {r}")),
    }
}

/// The ref inside the engine's branch label (`merge base with origin/main`).
#[must_use]
pub fn branch_ref_from_label(label: &str) -> Option<&str> {
    label.strip_prefix("merge base with ").map(str::trim)
}

// ── keys ─────────────────────────────────────────────────────────────────────

/// The Diff viewer's keys. Bare keys, live only while the diff has focus —
/// never menu shortcuts, which would swallow terminal input.
pub const KEYS: [(&str, &str); 11] = [
    ("j", "next_line"),
    ("k", "previous_line"),
    ("n", "next_hunk"),
    ("p", "previous_hunk"),
    ("]", "next_file"),
    ("[", "previous_file"),
    ("v", "toggle_viewed"),
    ("c", "comment"),
    ("r", "revert_hunk"),
    ("s", "stage_hunk"),
    ("u", "toggle_split"),
];

/// The action a bare key fires in the Diff viewer.
#[must_use]
pub fn key_action(key: &str) -> Option<&'static str> {
    KEYS.iter().find(|(k, _)| *k == key).map(|(_, a)| *a)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn l(kind: LineKind) -> Line {
        Line {
            kind,
            old: None,
            new: None,
            text: String::new(),
        }
    }

    pub fn file(path: &str, hunks: usize) -> File {
        File {
            path: path.into(),
            old_path: None,
            status: "modified".into(),
            added: 1,
            deleted: 0,
            binary: false,
            noise: None,
            collapsed_by_default: false,
            hunks_omitted: None,
            hunks: (0..hunks).map(|i| Hunk { id: format!("{path}#{i}"), lines: vec![] }).collect(),
        }
    }

    #[test]
    fn pairs_change_blocks() {
        use LineKind::{Add, Ctx, Del};
        let lines: Vec<Line> = [Ctx, Del, Del, Add, Ctx, Add, Del].into_iter().map(l).collect();
        let rows = side_by_side(&lines);
        let pairs: Vec<(Option<usize>, Option<usize>)> = rows.iter().map(|r| (r.left, r.right)).collect();
        assert_eq!(
            pairs,
            vec![(Some(0), Some(0)), (Some(1), Some(3)), (Some(2), None), (Some(4), Some(4)), (None, Some(5)), (Some(6), None)]
        );
    }

    #[test]
    fn tree_sorts_dirs_first_and_compresses_chains() {
        let paths = ["src/b.rs", "README.md", "src/a.rs", "apps/x/y/z.swift", "Cargo.toml", "src/ui/v.rs"];
        let names: Vec<(String, usize)> = tree(&paths).into_iter().map(|r| (r.name, r.depth)).collect();
        assert_eq!(
            names,
            vec![
                ("apps/x/y".into(), 0),
                ("z.swift".into(), 1),
                ("src".into(), 0),
                ("ui".into(), 1),
                ("v.rs".into(), 2),
                ("a.rs".into(), 1),
                ("b.rs".into(), 1),
                ("Cargo.toml".into(), 0),
                ("README.md".into(), 0),
            ]
        );
        assert_eq!(file_order(&paths), vec![3, 5, 2, 0, 4, 1]);
    }

    #[test]
    fn navigation_skips_collapsed_and_stops_at_ends() {
        let files = vec![file("a", 2), file("b", 1), file("c", 1)];
        let order = [0, 1, 2];
        let collapsed = [false, true, false];
        assert_eq!(next_hunk(&files, &order, &collapsed, None, true), Some((0, 0)));
        assert_eq!(next_hunk(&files, &order, &collapsed, Some((0, 1)), true), Some((2, 0)), "b is collapsed");
        assert_eq!(next_hunk(&files, &order, &collapsed, Some((2, 0)), true), None);
        assert_eq!(next_hunk(&files, &order, &collapsed, None, false), Some((2, 0)));
        assert_eq!(next_hunk(&files, &order, &collapsed, Some((0, 0)), false), None);
        assert_eq!(next_file(&order, Some(0), true), Some(1));
        assert_eq!(next_file(&order, Some(2), true), None);
        assert_eq!(next_file(&order, None, false), Some(2));
    }

    #[test]
    fn collapsed_reasons_and_viewed_keys() {
        let mut f = file("Cargo.lock", 0);
        f.noise = Some("lockfile".into());
        f.collapsed_by_default = true;
        f.hunks_omitted = Some("collapsed".into());
        assert_eq!(display(&f, false).reason.as_deref(), Some("noise"));
        assert!(display(&f, false).fetch);
        assert_eq!(display(&file("a", 1), true).reason.as_deref(), Some("viewed"));
        assert!(!display(&file("a", 1), false).collapsed);
        let a = file("a", 1);
        let mut b = a.clone();
        b.hunks[0].id = "other".into();
        assert_ne!(viewed_key(&a), viewed_key(&b), "a changed file comes back unviewed");
        assert_eq!(default_base("shell"), Base::Uncommitted);
        assert_eq!(default_base("claude"), Base::Turn);
        assert_eq!(base_label(Base::Branch, Some("origin/main")), "vs main");
        assert_eq!(base_label(Base::Branch, None), "vs default branch");
        assert_eq!(branch_ref_from_label("merge base with origin/main"), Some("origin/main"));
        assert_eq!(key_action("n"), Some("next_hunk"));
        assert_eq!(key_action("x"), None);
    }
}
