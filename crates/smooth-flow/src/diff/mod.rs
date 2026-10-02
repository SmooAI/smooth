//! SmoothFlow Diff (epic th-26f5b9): structured diffs the clients render
//! natively — files, hunks, lines, word-level change spans and syntax token
//! spans, all computed here so no client runs `git` or a highlighter.
//!
//! Three bases ([`DiffBase`]):
//!
//! - **`turn`** — what the agent's last turn changed. The engine snapshots
//!   the worktree as a git tree at each turn boundary (see [`snapshot`]);
//!   an idle agent's turn diff is its last turn's start tree → end tree, so
//!   your own edits since then don't show up as the agent's; while a turn
//!   is running it is the start tree → the worktree now (`turn.live`).
//! - **`uncommitted`** — `HEAD` → the worktree, untracked files included.
//! - **`branch`** — the merge base with the default branch → the worktree.
//!
//! Every base is a tree-to-tree `git diff`; "the worktree now" is itself a
//! snapshot tree, so untracked files need no special case. Hunk actions
//! (revert / stage / unstage) recompute the diff, find the hunk by its
//! stable id, and `git apply` a one-hunk patch — refusing, with nothing
//! written, when it no longer applies.

pub mod git;
pub mod model;
pub mod noise;
pub mod parse;
pub mod snapshot;
pub mod syntax;
pub mod words;

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

pub use model::{Diff, DiffBase, DiffFile, DiffSide, FileStatus, Hunk, Line, LineKind, Noise, Omitted, ReviewComment, TokenKind, TurnInfo};
pub use noise::LARGE_FILE_LINES;
pub use snapshot::{SnapKind, Snapshot};

use parse::{RawFile, RawHunk};

/// Files past this are not listed (`Diff::files_omitted`).
pub const MAX_FILES: usize = 1500;
/// Lines shown per file before the rest is cut (`DiffFile::truncated`).
pub const MAX_FILE_LINES: usize = 3000;
/// Lines shown per hunk before the rest is cut (`Hunk::truncated`).
pub const MAX_HUNK_LINES: usize = 1000;
/// Chars shown per line before the rest is cut (`Line::truncated`).
pub const MAX_LINE_CHARS: usize = 2000;
/// The serialized size one `flow.diff` frame aims under. The relay and the
/// phones' WebSocket stacks cap a message near 1 MiB (URLSession's default),
/// and end-to-end encryption adds a third in base64; 512 KiB of JSON leaves
/// room for both. Files past the budget arrive as stubs
/// (`hunks_omitted: budget`) to fetch one at a time with `path`.
pub const FRAME_BUDGET_BYTES: usize = 512 * 1024;
/// Wall-clock spent on syntax spans per diff; past it, lines arrive uncolored.
pub const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(750);

/// A hunk action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkAction {
    Revert,
    Stage,
    Unstage,
}

impl HunkAction {
    /// The wire spelling (`flow.diff.<action>`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Revert => "revert",
            Self::Stage => "stage",
            Self::Unstage => "unstage",
        }
    }
}

/// Both ends of a diff, resolved to tree/commit ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub from: DiffSide,
    pub to: DiffSide,
    pub turn: Option<TurnInfo>,
    pub note: Option<String>,
}

fn side(r: impl Into<String>, label: impl Into<String>) -> DiffSide {
    DiffSide {
        r#ref: r.into(),
        label: label.into(),
    }
}

/// Resolve `base` for the worktree at `dir`, given the session's turn
/// snapshots (oldest first). Takes a fresh worktree snapshot when the right
/// side is "now".
///
/// # Errors
/// Outside a repo, or when git refuses.
pub fn resolve(dir: &Path, base: DiffBase, snaps: &[Snapshot]) -> Result<Resolved> {
    if !git::is_repo(dir) {
        bail!("{} is not in a git repository", dir.display());
    }
    let empty = || git::empty_tree(dir);
    match base {
        DiffBase::Uncommitted => {
            let from = git::head_tree(dir).map_or_else(|| Ok::<_, anyhow::Error>(side(empty()?, "empty repository")), |t| Ok(side(t, "HEAD")))?;
            Ok(Resolved {
                from,
                to: side(git::snapshot_tree(dir)?, "worktree"),
                turn: None,
                note: None,
            })
        }
        DiffBase::Branch => {
            let (from, note) = match git::branch_base(dir) {
                Some((r, sha)) => (side(sha, format!("merge base with {r}")), None),
                None => match git::head_tree(dir) {
                    Some(t) => (side(t, "HEAD"), Some("No default branch found — showing changes against HEAD.".to_string())),
                    None => (side(empty()?, "empty repository"), None),
                },
            };
            Ok(Resolved {
                from,
                to: side(git::snapshot_tree(dir)?, "worktree"),
                turn: None,
                note,
            })
        }
        DiffBase::Turn => resolve_turn(dir, snaps),
    }
}

fn resolve_turn(dir: &Path, snaps: &[Snapshot]) -> Result<Resolved> {
    let none = |note: &str| -> Result<Resolved> {
        let t = git::head_tree(dir).map_or_else(|| git::empty_tree(dir), Ok)?;
        Ok(Resolved {
            from: side(t.clone(), "no turn"),
            to: side(t, "no turn"),
            turn: None,
            note: Some(note.to_string()),
        })
    };
    let Some(last) = snaps.last() else {
        return none("No turn recorded yet — SmoothFlow snapshots the worktree when the agent starts and finishes a turn.");
    };
    let pick = |s: &Snapshot| -> Result<String> {
        if git::object_exists(dir, &s.tree) {
            Ok(s.tree.clone())
        } else {
            Err(anyhow!("turn snapshot {} was pruned by git gc", s.tree))
        }
    };
    match last.kind {
        SnapKind::Start => Ok(Resolved {
            from: side(pick(last)?, "turn start"),
            to: side(git::snapshot_tree(dir)?, "worktree"),
            turn: Some(TurnInfo {
                seq: last.seq,
                live: true,
                started_at: Some(last.at.clone()),
                ended_at: None,
            }),
            note: None,
        }),
        SnapKind::End => {
            let prev = snaps.len().checked_sub(2).map(|i| &snaps[i]);
            let Some(prev) = prev else {
                return none("Only one turn snapshot so far — the next turn will have a diff.");
            };
            Ok(Resolved {
                from: side(pick(prev)?, if prev.kind == SnapKind::Start { "turn start" } else { "previous turn end" }),
                to: side(pick(last)?, "turn end"),
                turn: Some(TurnInfo {
                    seq: last.seq,
                    live: false,
                    started_at: Some(prev.at.clone()),
                    ended_at: Some(last.at.clone()),
                }),
                note: None,
            })
        }
    }
}

/// Parsed raw files of a resolved diff, plus whether git's output was cut.
///
/// # Errors
/// When git refuses.
pub fn raw(dir: &Path, r: &Resolved) -> Result<(Vec<RawFile>, bool)> {
    if r.from.r#ref == r.to.r#ref {
        return Ok((Vec::new(), false));
    }
    let (patch, truncated) = git::diff_trees(dir, &r.from.r#ref, &r.to.r#ref)?;
    Ok((parse::parse(&patch), truncated))
}

/// Hunk ids already in the index (`uncommitted` only): HEAD → index.
fn staged_ids(dir: &Path, r: &Resolved) -> HashSet<String> {
    let Ok(index) = git::index_tree(dir) else { return HashSet::new() };
    if index == r.from.r#ref {
        return HashSet::new();
    }
    git::diff_trees(dir, &r.from.r#ref, &index)
        .map(|(p, _)| parse::parse(&p).iter().flat_map(RawFile::hunk_ids).collect())
        .unwrap_or_default()
}

/// The structured diff of `dir` against `base`. `path` narrows it to one
/// file (a client expanding a collapsed or budget-stubbed file): that file
/// arrives with its hunks even when it is noise.
///
/// # Errors
/// Outside a repo, or when git refuses.
pub fn compute(dir: &Path, base: DiffBase, snaps: &[Snapshot], path: Option<&str>) -> Result<Diff> {
    let r = resolve(dir, base, snaps)?;
    let (files, truncated) = raw(dir, &r)?;
    let staged = if base == DiffBase::Uncommitted { staged_ids(dir, &r) } else { HashSet::new() };
    let mut d = build(&files, truncated, &staged, path, FRAME_BUDGET_BYTES);
    d.base = base;
    d.from = r.from;
    d.to = r.to;
    d.turn = r.turn;
    d.note = r.note.or(d.note);
    if let Some(p) = path {
        if d.files.is_empty() {
            bail!("no change to {p} in the {} diff", base.as_str());
        }
    }
    Ok(d)
}

fn strip_cr(s: &str) -> (&str, bool) {
    s.strip_suffix('\r').map_or((s, false), |t| (t, true))
}

fn cut(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        // Fast path: bytes ≤ max ⇒ chars ≤ max.
        return (s.to_string(), false);
    }
    match s.char_indices().nth(max) {
        Some((i, _)) => (s[..i].to_string(), true),
        None => (s.to_string(), false),
    }
}

fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Build one display hunk from a raw one: line numbers, CR, caps, words, syntax.
fn build_hunk(raw: &RawHunk, id: String, limit: usize, syntax: Option<&'static syntect::parsing::SyntaxReference>, deadline: Instant) -> Hunk {
    let take = raw.lines.len().min(limit);
    let texts: Vec<(LineKind, &str)> = raw.lines[..take].iter().map(|l| (l.kind, strip_cr(&l.text).0)).collect();
    let words = words::hunk_word_spans(&texts);
    let mut old_hl = syntax.map(syntax::Highlighter::new);
    let mut new_hl = syntax.map(syntax::Highlighter::new);
    let (mut old_no, mut new_no) = (raw.old_start, raw.new_start);
    let mut lines = Vec::with_capacity(take);
    for (l, w) in raw.lines[..take].iter().zip(words) {
        let (text, cr) = strip_cr(&l.text);
        let in_time = Instant::now() < deadline;
        let spans = |h: &mut Option<syntax::Highlighter>| if in_time { h.as_mut().map(|h| h.line(text)).unwrap_or_default() } else { Vec::new() };
        let (old, new, syn) = match l.kind {
            LineKind::Ctx => {
                let _ = spans(&mut old_hl);
                let s = spans(&mut new_hl);
                let r = (Some(old_no), Some(new_no), s);
                old_no += 1;
                new_no += 1;
                r
            }
            LineKind::Del => {
                let r = (Some(old_no), None, spans(&mut old_hl));
                old_no += 1;
                r
            }
            LineKind::Add => {
                let r = (None, Some(new_no), spans(&mut new_hl));
                new_no += 1;
                r
            }
        };
        let (shown, truncated) = cut(text, MAX_LINE_CHARS);
        let max = u32_of(shown.chars().count());
        let syn: Vec<[u32; 3]> = syn.into_iter().filter(|s| s[0] < max).map(|[s, e, k]| [s, e.min(max), k]).collect();
        let w: Vec<[u32; 2]> = w.into_iter().filter(|s| s[0] < max).map(|[s, e]| [s, e.min(max)]).collect();
        lines.push(Line {
            kind: l.kind,
            old,
            new,
            text: shown,
            no_eol: l.no_eol,
            cr,
            truncated,
            syntax: syn,
            words: w,
        });
    }
    Hunk {
        id,
        old_start: raw.old_start,
        old_lines: raw.old_lines,
        new_start: raw.new_start,
        new_lines: raw.new_lines,
        section: raw.section.clone(),
        lines,
        truncated: take < raw.lines.len(),
        staged: false,
    }
}

/// Turn raw files into the display model: counts, noise, caps, word and
/// syntax spans, staged marks, and the frame budget. Pure apart from the
/// highlight clock.
#[must_use]
pub fn build(files: &[RawFile], git_truncated: bool, staged: &HashSet<String>, path: Option<&str>, budget: usize) -> Diff {
    let deadline = Instant::now() + HIGHLIGHT_BUDGET;
    let (mut added, mut deleted) = (0u32, 0u32);
    for f in files {
        let (a, d) = f.counts();
        added = added.saturating_add(a);
        deleted = deleted.saturating_add(d);
    }
    let selected: Vec<&RawFile> = match path {
        Some(p) => files.iter().filter(|f| f.path() == p || f.old_path.as_deref() == Some(p)).take(1).collect(),
        None => files.iter().take(MAX_FILES).collect(),
    };
    let mut out_files = Vec::with_capacity(selected.len());
    for f in selected {
        let (a, d) = f.counts();
        let add_texts: Vec<&str> = f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == LineKind::Add).take(64).map(|l| l.text.as_str()).collect();
        let noise = noise::classify(f.path(), a, d, &add_texts);
        let syn = if f.binary { None } else { syntax::syntax_for(f.path()) };
        let mut df = DiffFile {
            path: f.path().to_string(),
            old_path: matches!(f.status, FileStatus::Renamed | FileStatus::Copied).then(|| f.old_path.clone()).flatten(),
            status: f.status,
            old_mode: f.old_mode.clone(),
            new_mode: f.new_mode.clone(),
            binary: f.binary,
            language: syn.map(|s| s.name.clone()),
            added: a,
            deleted: d,
            noise,
            collapsed_by_default: noise.is_some(),
            hunks_omitted: None,
            truncated: false,
            hunks: Vec::new(),
        };
        if path.is_none() && noise.is_some() && !f.hunks.is_empty() {
            df.hunks_omitted = Some(Omitted::Collapsed);
            out_files.push(df);
            continue;
        }
        // Noise that was asked for by path is shown, but not colored.
        let syn = if noise.is_some() { None } else { syn };
        let ids = f.hunk_ids();
        let mut used = 0usize;
        for (h, id) in f.hunks.iter().zip(ids) {
            if used >= MAX_FILE_LINES {
                df.truncated = true;
                break;
            }
            let limit = MAX_HUNK_LINES.min(MAX_FILE_LINES - used);
            let mut hunk = build_hunk(h, id, limit, syn, deadline);
            hunk.staged = staged.contains(&hunk.id);
            used += hunk.lines.len();
            df.truncated |= hunk.truncated;
            df.hunks.push(hunk);
        }
        out_files.push(df);
    }
    fit_budget(&mut out_files, path.is_some(), budget);
    Diff {
        base: DiffBase::Uncommitted,
        from: side("", ""),
        to: side("", ""),
        turn: None,
        note: None,
        files: out_files,
        added,
        deleted,
        truncated: git_truncated || (path.is_none() && files.len() > MAX_FILES),
        files_omitted: if path.is_none() { u32_of(files.len().saturating_sub(MAX_FILES)) } else { 0 },
        legend: TokenKind::legend(),
    }
}

fn json_len<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_vec(v).map_or(0, |b| b.len())
}

/// Keep the frame under `budget`: in a full listing a file that doesn't fit
/// becomes a stub; a single requested file loses its tail hunks instead.
fn fit_budget(files: &mut [DiffFile], single: bool, budget: usize) {
    if single {
        for f in files.iter_mut() {
            let mut used = 1024;
            let mut keep = 0;
            for h in &f.hunks {
                let n = json_len(h);
                if used + n > budget && keep > 0 {
                    break;
                }
                used += n;
                keep += 1;
            }
            if keep < f.hunks.len() {
                f.hunks.truncate(keep);
                f.truncated = true;
            }
        }
        return;
    }
    let mut used = 1024;
    for f in files.iter_mut() {
        let n = json_len(f);
        if used + n > budget && !f.hunks.is_empty() {
            f.hunks.clear();
            f.hunks_omitted = Some(Omitted::Budget);
            used += json_len(f);
        } else {
            used += n;
        }
    }
}

/// Find a hunk by id among raw files: `(file, hunk)`.
#[must_use]
pub fn find_hunk<'a>(files: &'a [RawFile], hunk_id: &str) -> Option<(&'a RawFile, &'a RawHunk)> {
    files.iter().find_map(|f| f.hunk_ids().iter().position(|id| id == hunk_id).map(|i| (f, &f.hunks[i])))
}

/// What a hunk action did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionOutcome {
    pub action: String,
    pub hunk_id: String,
    pub file: String,
}

/// Revert, stage or unstage one hunk of the `base` diff.
///
/// Revert reverse-applies the hunk to the worktree on any base. Stage and
/// unstage are index operations and only make sense against the
/// `uncommitted` view; unstage looks the hunk up in HEAD → index.
///
/// # Errors
/// `stale:` when the hunk is gone or no longer applies (nothing written);
/// binary files and stage/unstage on another base are refused.
pub fn act(dir: &Path, base: DiffBase, snaps: &[Snapshot], hunk_id: &str, action: HunkAction) -> Result<ActionOutcome> {
    if action != HunkAction::Revert && base != DiffBase::Uncommitted {
        bail!("{} works on the uncommitted view only (the index is HEAD-relative)", action.as_str());
    }
    let r = resolve(dir, base, snaps)?;
    let files = if action == HunkAction::Unstage {
        let index = git::index_tree(dir)?;
        let staged = Resolved {
            to: side(index, "index"),
            ..r
        };
        raw(dir, &staged)?.0
    } else {
        raw(dir, &r)?.0
    };
    let Some((file, hunk)) = find_hunk(&files, hunk_id) else {
        bail!("stale: hunk {hunk_id} is not in the current {} diff — refresh", base.as_str());
    };
    if file.binary {
        bail!("binary files have no hunks to {}", action.as_str());
    }
    let patch = parse::hunk_patch(file, hunk);
    let target = match action {
        HunkAction::Revert => git::ApplyTarget::WorktreeReverse,
        HunkAction::Stage => git::ApplyTarget::Index,
        HunkAction::Unstage => git::ApplyTarget::IndexReverse,
    };
    git::apply(dir, &patch, target)?;
    Ok(ActionOutcome {
        action: action.as_str().to_string(),
        hunk_id: hunk_id.to_string(),
        file: file.path().to_string(),
    })
}

/// Most excerpt lines quoted per comment.
pub const REVIEW_EXCERPT_LINES: usize = 12;

fn excerpt(file: &RawFile, c: &ReviewComment) -> Vec<String> {
    let ids = file.hunk_ids();
    let hunks: Vec<&RawHunk> = match c.hunk_id.as_deref() {
        Some(id) => ids.iter().position(|x| x == id).map(|i| vec![&file.hunks[i]]).unwrap_or_default(),
        None => file.hunks.iter().collect(),
    };
    let old_side = c.side.as_deref() == Some("old");
    for h in hunks {
        let (mut o, mut n) = (h.old_start, h.new_start);
        let mut numbered = Vec::with_capacity(h.lines.len());
        for l in &h.lines {
            let (lo, ln) = match l.kind {
                LineKind::Ctx => (Some(o), Some(n)),
                LineKind::Del => (Some(o), None),
                LineKind::Add => (None, Some(n)),
            };
            if lo.is_some() {
                o += 1;
            }
            if ln.is_some() {
                n += 1;
            }
            numbered.push((l, if old_side { lo } else { ln }));
        }
        let hits: Vec<usize> = match c.line_range {
            Some([a, b]) => numbered.iter().enumerate().filter(|(_, (_, no))| no.is_some_and(|x| x >= a.min(b) && x <= a.max(b))).map(|(i, _)| i).collect(),
            None => (0..numbered.len()).collect(),
        };
        let (Some(&first), Some(&last)) = (hits.first(), hits.last()) else { continue };
        let mut out: Vec<String> = numbered[first..=last]
            .iter()
            .map(|(l, _)| {
                let p = match l.kind {
                    LineKind::Add => '+',
                    LineKind::Del => '-',
                    LineKind::Ctx => ' ',
                };
                let (t, _) = strip_cr(&l.text);
                let (t, cut_) = cut(t, 200);
                format!("{p}{t}{}", if cut_ { "…" } else { "" })
            })
            .collect();
        if out.len() > REVIEW_EXCERPT_LINES {
            let more = out.len() - REVIEW_EXCERPT_LINES;
            out.truncate(REVIEW_EXCERPT_LINES);
            out.push(format!("… ({more} more lines)"));
        }
        return out;
    }
    Vec::new()
}

/// The single steer message a batch of review comments becomes: every
/// location, its hunk excerpt, and the comment, numbered.
#[must_use]
pub fn review_message(base: DiffBase, files: &[RawFile], comments: &[ReviewComment]) -> String {
    let what = match base {
        DiffBase::Turn => "your last turn",
        DiffBase::Uncommitted => "the uncommitted changes",
        DiffBase::Branch => "this branch's changes",
    };
    let n = comments.len();
    let mut out = format!(
        "Code review of {what} from SmoothFlow — {n} comment{}. Please address each one:\n",
        if n == 1 { "" } else { "s" }
    );
    for (i, c) in comments.iter().enumerate() {
        let loc = match c.line_range {
            Some([a, b]) if a.min(b) == a.max(b) => format!("{}:{}", c.file, a),
            Some([a, b]) => format!("{}:{}-{}", c.file, a.min(b), a.max(b)),
            None => c.file.clone(),
        };
        let side = if c.side.as_deref() == Some("old") { " (old side)" } else { "" };
        let _ = write!(out, "\n{}. {loc}{side}\n", i + 1);
        let file = files.iter().find(|f| f.path() == c.file || f.old_path.as_deref() == Some(c.file.as_str()));
        let ex = file.map(|f| excerpt(f, c)).unwrap_or_default();
        if ex.is_empty() && c.hunk_id.is_some() {
            out.push_str("   (that hunk has changed since the comment was written)\n");
        }
        if !ex.is_empty() {
            out.push_str("   ```diff\n");
            for l in ex {
                let _ = writeln!(out, "   {l}");
            }
            out.push_str("   ```\n");
        }
        for l in c.text.trim().lines() {
            let _ = writeln!(out, "   {l}");
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests;
