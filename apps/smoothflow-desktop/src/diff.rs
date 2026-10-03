//! The Diff tab, toolkit-free (Client Spec §14, th-26f5b9): the `flow.diff`
//! payload, the viewer's state, the rows it draws, and what each key and
//! button does. The engine computes everything (hunks, word spans, syntax
//! spans); the shared rules (`smooth_flow_client::diff`) decide tree order,
//! what starts collapsed, navigation and the default base. `view` only draws
//! [`Viewer::rows`].

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use smooth_flow_client::diff::{self as rules, Base, LineKind};

use crate::field::{Edit, Field};
use crate::frames;
use crate::net::Outbox;

// ── the wire ─────────────────────────────────────────────────────────────────

/// One side of the comparison (`from` / `to`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Side {
    #[serde(default, rename = "ref")]
    pub r#ref: String,
    #[serde(default)]
    pub label: String,
}

/// Which turn a `turn` diff shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Turn {
    #[serde(default)]
    pub seq: i64,
    /// The turn is still running.
    #[serde(default)]
    pub live: bool,
}

/// One line of a hunk. Offsets in `syntax` / `words` are Unicode scalars.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Line {
    pub kind: LineKind,
    #[serde(default)]
    pub old: Option<u32>,
    #[serde(default)]
    pub new: Option<u32>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub no_eol: bool,
    #[serde(default)]
    pub truncated: bool,
    /// `[start, end, kind]`; `kind` indexes [`Payload::legend`].
    #[serde(default)]
    pub syntax: Vec<[u32; 3]>,
    /// `[start, end)` changed against the paired line.
    #[serde(default)]
    pub words: Vec<[u32; 2]>,
}

/// One hunk.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Hunk {
    pub id: String,
    #[serde(default)]
    pub old_start: u32,
    #[serde(default)]
    pub old_lines: u32,
    #[serde(default)]
    pub new_start: u32,
    #[serde(default)]
    pub new_lines: u32,
    #[serde(default)]
    pub section: String,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub truncated: bool,
    /// `uncommitted` only: already in the index.
    #[serde(default)]
    pub staged: bool,
}

impl Hunk {
    /// `@@ -a,b +c,d @@ section`
    #[must_use]
    pub fn header(&self) -> String {
        let mut h = format!("@@ -{},{} +{},{} @@", self.old_start, self.old_lines, self.new_start, self.new_lines);
        if !self.section.is_empty() {
            h.push(' ');
            h.push_str(&self.section);
        }
        h
    }
}

/// One file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct File {
    pub path: String,
    #[serde(default)]
    pub old_path: Option<String>,
    /// added | deleted | modified | renamed | copied | mode_changed
    #[serde(default = "modified")]
    pub status: String,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub deleted: u32,
    #[serde(default)]
    pub noise: Option<String>,
    #[serde(default)]
    pub collapsed_by_default: bool,
    /// `collapsed` | `budget`: the hunks are not in this payload.
    #[serde(default)]
    pub hunks_omitted: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub hunks: Vec<Hunk>,
}

fn modified() -> String {
    "modified".into()
}

impl File {
    /// The tree's one-letter status badge.
    #[must_use]
    pub fn badge(&self) -> &'static str {
        match self.status.as_str() {
            "added" => "A",
            "deleted" => "D",
            "renamed" => "R",
            "copied" => "C",
            "mode_changed" => "X",
            _ => "M",
        }
    }

    /// The slice of this file the shared rules read (hunk ids and line
    /// kinds; no text).
    #[must_use]
    pub fn rule(&self) -> rules::File {
        rules::File {
            path: self.path.clone(),
            old_path: self.old_path.clone(),
            status: self.status.clone(),
            added: self.added,
            deleted: self.deleted,
            binary: self.binary,
            noise: self.noise.clone(),
            collapsed_by_default: self.collapsed_by_default,
            hunks_omitted: self.hunks_omitted.clone(),
            hunks: self
                .hunks
                .iter()
                .map(|h| rules::Hunk {
                    id: h.id.clone(),
                    lines: Vec::new(),
                })
                .collect(),
        }
    }
}

/// The whole `diff` of a `flow.diff` reply.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Payload {
    pub base: Base,
    #[serde(default)]
    pub from: Side,
    #[serde(default)]
    pub to: Side,
    #[serde(default)]
    pub turn: Option<Turn>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub files: Vec<File>,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub deleted: u32,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub files_omitted: u32,
    #[serde(default)]
    pub legend: Vec<String>,
}

impl Payload {
    fn order(&self) -> Vec<usize> {
        let paths: Vec<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
        rules::file_order(&paths)
    }

    /// The file tree beside the diff.
    #[must_use]
    pub fn tree(&self) -> Vec<rules::TreeRow> {
        let paths: Vec<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
        rules::tree(&paths)
    }
}

/// The wire spelling of a base.
#[must_use]
pub const fn base_str(b: Base) -> &'static str {
    match b {
        Base::Turn => "turn",
        Base::Uncommitted => "uncommitted",
        Base::Branch => "branch",
    }
}

/// The base picker, in order.
pub const BASES: [Base; 3] = [Base::Turn, Base::Uncommitted, Base::Branch];

// ── colors ───────────────────────────────────────────────────────────────────

/// Catppuccin Mocha, the default theme (same as the Mac's `DiffPalette`).
pub mod palette {
    pub const TEXT: u32 = 0xcdd6f4;
    pub const OVERLAY0: u32 = 0x6c7086;
    pub const OVERLAY2: u32 = 0x9399b2;
    pub const GREEN: u32 = 0xa6e3a1;
    pub const RED: u32 = 0xf38ba8;
    pub const MAUVE: u32 = 0xcba6f7;
    pub const PEACH: u32 = 0xfab387;
    pub const YELLOW: u32 = 0xf9e2af;
    pub const BLUE: u32 = 0x89b4fa;
    pub const SKY: u32 = 0x89dceb;
    pub const LAVENDER: u32 = 0xb4befe;
    pub const PINK: u32 = 0xf5c2e7;
    pub const ROSEWATER: u32 = 0xf5e0dc;

    /// A syntax token kind (by `legend` name) → its color; `None` is plain text.
    #[must_use]
    pub fn token(name: &str) -> Option<u32> {
        Some(match name {
            "keyword" => MAUVE,
            "string" => GREEN,
            "comment" | "punctuation" => OVERLAY2,
            "number" | "constant" => PEACH,
            "function" | "tag" => BLUE,
            "type" | "attribute" => YELLOW,
            "variable" => TEXT,
            "property" => LAVENDER,
            "operator" => SKY,
            "macro" | "link" => ROSEWATER,
            "escape" => PINK,
            "heading" => RED,
            _ => return None,
        })
    }

    /// The tree badge's color for a file status.
    #[must_use]
    pub fn badge(status: &str) -> u32 {
        match status {
            "added" => GREEN,
            "deleted" => RED,
            "renamed" | "copied" => BLUE,
            "mode_changed" => OVERLAY2,
            _ => PEACH,
        }
    }
}

/// A styled run of a line: `len` bytes of the text in `fg`, with the
/// stronger word-change background when `word`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub len: usize,
    pub fg: u32,
    pub word: bool,
}

/// A line as text plus runs: syntax colors from the engine's spans (scalar
/// offsets, clamped to the line), word spans on add/del lines, then the
/// truncation and missing-newline markers.
#[must_use]
pub fn styled(l: &Line, legend: &[String]) -> (String, Vec<Run>) {
    let chars: Vec<char> = l.text.chars().collect();
    let mut fg = vec![palette::TEXT; chars.len()];
    let mut word = vec![false; chars.len()];
    let clamp = |a: u32, b: u32| {
        let (a, b) = (a as usize, b as usize);
        (a.min(chars.len()), b.min(chars.len()))
    };
    for [s, e, k] in &l.syntax {
        let Some(color) = legend.get(*k as usize).and_then(|n| palette::token(n)) else {
            continue;
        };
        let (s, e) = clamp(*s, *e);
        fg.iter_mut().take(e).skip(s).for_each(|c| *c = color);
    }
    if l.kind != LineKind::Ctx {
        for [s, e] in &l.words {
            let (s, e) = clamp(*s, *e);
            word.iter_mut().take(e).skip(s).for_each(|w| *w = true);
        }
    }
    let mut runs: Vec<Run> = Vec::new();
    for (i, ch) in chars.iter().enumerate() {
        let len = ch.len_utf8();
        match runs.last_mut() {
            Some(r) if r.fg == fg[i] && r.word == word[i] => r.len += len,
            _ => runs.push(Run { len, fg: fg[i], word: word[i] }),
        }
    }
    let mut text = l.text.clone();
    let mut suffix = |s: &str, color: u32| {
        text.push_str(s);
        runs.push(Run {
            len: s.len(),
            fg: color,
            word: false,
        });
    };
    if l.truncated {
        suffix(" …", palette::OVERLAY0);
    }
    if l.no_eol {
        suffix("  (no newline at end of file)", palette::RED);
    }
    (text, runs)
}

// ── rows ─────────────────────────────────────────────────────────────────────

/// One row of the main pane. Rows are computed, never views: the list is
/// virtualized, so only what is on screen is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Banner(String),
    FileHeader {
        file: usize,
    },
    /// Why a file shows no lines; `fetch` = its hunks are not loaded yet.
    Notice {
        file: usize,
        text: String,
        fetch: bool,
    },
    HunkHeader {
        file: usize,
        hunk: usize,
    },
    /// Unified: one line.
    Line {
        file: usize,
        hunk: usize,
        line: usize,
    },
    /// Side by side: indexes into the hunk's lines for each column.
    Pair {
        file: usize,
        hunk: usize,
        left: Option<usize>,
        right: Option<usize>,
    },
    /// A pending review comment, under the line it is about.
    Comment {
        file: usize,
        index: usize,
    },
}

impl Row {
    #[must_use]
    pub const fn file(&self) -> Option<usize> {
        match self {
            Self::Banner(_) => None,
            Self::FileHeader { file }
            | Self::Notice { file, .. }
            | Self::HunkHeader { file, .. }
            | Self::Line { file, .. }
            | Self::Pair { file, .. }
            | Self::Comment { file, .. } => Some(*file),
        }
    }

    #[must_use]
    pub const fn hunk(&self) -> Option<usize> {
        match self {
            Self::HunkHeader { hunk, .. } | Self::Line { hunk, .. } | Self::Pair { hunk, .. } => Some(*hunk),
            _ => None,
        }
    }

    /// A row `j` / `k` stop on.
    #[must_use]
    pub const fn is_line(&self) -> bool {
        matches!(self, Self::Line { .. } | Self::Pair { .. })
    }
}

/// A review comment, kept here until "Send review".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub file: String,
    pub hunk_id: Option<String>,
    /// Inclusive line numbers on `side`.
    pub range: Option<(u32, u32)>,
    /// `new` or `old`.
    pub side: &'static str,
    pub text: String,
}

impl Comment {
    /// Where it points, as the draft and the row say it.
    #[must_use]
    pub fn place(&self) -> String {
        let at = match self.range {
            Some((a, b)) if a == b => format!("{}:{a}", self.file),
            Some((a, b)) => format!("{}:{a}-{b}", self.file),
            None => self.file.clone(),
        };
        if self.side == "old" {
            format!("{at} (old side)")
        } else {
            at
        }
    }

    fn wire(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "file": self.file, "text": self.text, "side": self.side });
        if let Some(h) = &self.hunk_id {
            v["hunk_id"] = serde_json::json!(h);
        }
        if let Some((a, b)) = self.range {
            v["line_range"] = serde_json::json!([a, b]);
        }
        v
    }

    /// Whether this comment sits under line `li` of hunk `hi` of `f`: the
    /// last line of its range, on its side.
    fn anchors(&self, f: &File, hi: usize, li: usize) -> bool {
        let Some((_, last)) = self.range else { return false };
        if self.file != f.path || self.hunk_id.as_deref().is_some_and(|id| f.hunks[hi].id != id) {
            return false;
        }
        let l = &f.hunks[hi].lines[li];
        (if self.side == "old" { l.old } else { l.new }) == Some(last)
    }
}

/// The notice for a collapsed or content-less file.
#[must_use]
pub fn notice_text(f: &File, d: &rules::Display) -> String {
    match d.reason.as_deref() {
        Some("viewed") => "Viewed".into(),
        Some("noise") => {
            let what = match f.noise.as_deref() {
                Some("lockfile") => "Lockfile",
                Some("vendored") => "Vendored code",
                Some("minified") => "Minified file",
                Some("large") => "Large change",
                _ => "Generated file",
            };
            format!("{what} · +{} −{} · collapsed", f.added, f.deleted)
        }
        Some("binary") => "Binary file".into(),
        Some("no_content") => match f.status.as_str() {
            "renamed" => format!("Renamed from {} with no content change", f.old_path.as_deref().unwrap_or("?")),
            "mode_changed" => "File mode changed".into(),
            _ => "No content change".into(),
        },
        _ if f.hunks_omitted.as_deref() == Some("budget") => format!("Large diff · +{} −{} · not loaded yet", f.added, f.deleted),
        _ => String::new(),
    }
}

/// Every row, files in tree order. `collapsed[i]` is file i's effective
/// state (the rules plus the user's toggles); `viewed[i]` its viewed mark.
#[must_use]
pub fn rows(diff: &Payload, split: bool, collapsed: &[bool], viewed: &[bool], comments: &[Comment]) -> Vec<Row> {
    let mut out = Vec::new();
    if let Some(note) = diff.note.as_deref().filter(|n| !n.is_empty()) {
        out.push(Row::Banner(note.to_string()));
    }
    if diff.truncated {
        let n = diff.files_omitted;
        out.push(Row::Banner(format!(
            "{n} more file{} not shown — the diff is too large to send whole.",
            if n == 1 { "" } else { "s" }
        )));
    }
    for fi in diff.order() {
        let f = &diff.files[fi];
        out.push(Row::FileHeader { file: fi });
        for (ci, c) in comments.iter().enumerate() {
            if c.file == f.path && c.range.is_none() {
                out.push(Row::Comment { file: fi, index: ci });
            }
        }
        let rule = rules::display(&f.rule(), viewed.get(fi).copied().unwrap_or(false));
        if collapsed.get(fi).copied().unwrap_or(rule.collapsed) {
            out.push(Row::Notice {
                file: fi,
                text: notice_text(f, &rule),
                fetch: rule.fetch,
            });
            continue;
        }
        if f.hunks.is_empty() {
            let text = if rule.fetch {
                format!("+{} −{} · loading…", f.added, f.deleted)
            } else {
                notice_text(
                    f,
                    &rules::Display {
                        collapsed: true,
                        reason: Some("no_content".into()),
                        fetch: false,
                    },
                )
            };
            out.push(Row::Notice {
                file: fi,
                text,
                fetch: rule.fetch,
            });
            continue;
        }
        for (hi, h) in f.hunks.iter().enumerate() {
            out.push(Row::HunkHeader { file: fi, hunk: hi });
            if split {
                let lines: Vec<rules::Line> = h
                    .lines
                    .iter()
                    .map(|l| rules::Line {
                        kind: l.kind,
                        old: l.old,
                        new: l.new,
                        text: String::new(),
                    })
                    .collect();
                for r in rules::side_by_side(&lines) {
                    out.push(Row::Pair {
                        file: fi,
                        hunk: hi,
                        left: r.left,
                        right: r.right,
                    });
                    for (ci, c) in comments.iter().enumerate() {
                        if [r.left, r.right].into_iter().flatten().any(|li| c.anchors(f, hi, li)) {
                            out.push(Row::Comment { file: fi, index: ci });
                        }
                    }
                }
            } else {
                for li in 0..h.lines.len() {
                    out.push(Row::Line { file: fi, hunk: hi, line: li });
                    for (ci, c) in comments.iter().enumerate() {
                        if c.anchors(f, hi, li) {
                            out.push(Row::Comment { file: fi, index: ci });
                        }
                    }
                }
            }
            if h.truncated {
                out.push(Row::Notice {
                    file: fi,
                    text: "Hunk truncated — the rest of it is too large to show".into(),
                    fetch: false,
                });
            }
        }
        if f.truncated && !f.hunks.iter().any(|h| h.truncated) {
            out.push(Row::Notice {
                file: fi,
                text: "File truncated — more changes than the viewer shows".into(),
                fetch: false,
            });
        }
    }
    if out.is_empty() {
        out.push(Row::Banner("No changes.".into()));
    }
    out
}

// ── the viewer ───────────────────────────────────────────────────────────────

/// What a request in flight was, so its `flow.error` (matched by `ref`) can
/// be told apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Diff { path: Option<String> },
    Action,
    Review,
}

/// A comment being written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub field: Field,
    pub target: Comment,
}

/// A Revert waiting for its confirmation (the caller shows the dialog).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertAsk {
    pub session: String,
    pub base: Base,
    pub hunk_id: String,
    pub title: String,
    pub message: String,
}

/// What a key in the Diff tab did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    Handled,
    /// `r`: ask before reverting.
    Revert(RevertAsk),
    /// Not a Diff key.
    Ignored,
}

/// The seqs this viewer stamps on its requests start here, clear of the
/// small numbers the rest of the app uses, so a `flow.error`'s `ref` names
/// exactly one requester.
const SEQ_BASE: u64 = 1 << 32;

/// The Diff tab's state: one session's diff at a time.
pub struct Viewer {
    out: Outbox,
    /// The session shown, and its kind.
    pub session: Option<String>,
    kind: String,
    pub base: Base,
    /// The base the user picked, per session (spec §14: remembered).
    chosen: HashMap<String, Base>,
    pub diff: Option<Payload>,
    /// The ref the last `branch` diff was against (for the picker label).
    branch_ref: Option<String>,
    /// Side by side instead of unified.
    pub split: bool,
    /// Per session: viewed keys, collapse overrides by path, pending comments.
    viewed: HashMap<String, HashSet<String>>,
    toggles: HashMap<String, HashMap<String, bool>>,
    comments: HashMap<String, Vec<Comment>>,
    pub rows: Vec<Row>,
    pub tree: Vec<rules::TreeRow>,
    pub cursor: Option<usize>,
    anchor: Option<usize>,
    /// The last result or engine refusal, verbatim; `error` colors it.
    pub status: Option<String>,
    pub error: bool,
    pub draft: Option<Draft>,
    /// A row the list should scroll to, and whether to its top (a file
    /// jump) rather than just into view (taken by the view).
    scroll_to: Option<(usize, bool)>,
    /// A whole-diff request is in flight; `again` = refetch when it lands.
    loading: bool,
    again: bool,
    /// Single-file pages requested and not yet answered.
    fetching: HashSet<String>,
    pending: HashMap<u64, Pending>,
    next_seq: u64,
}

impl Viewer {
    #[must_use]
    pub fn new(out: Outbox) -> Self {
        Self {
            out,
            session: None,
            kind: String::new(),
            base: Base::Turn,
            chosen: HashMap::new(),
            diff: None,
            branch_ref: None,
            split: false,
            viewed: HashMap::new(),
            toggles: HashMap::new(),
            comments: HashMap::new(),
            rows: Vec::new(),
            tree: Vec::new(),
            cursor: None,
            anchor: None,
            status: None,
            error: false,
            draft: None,
            scroll_to: None,
            loading: false,
            again: false,
            fetching: HashSet::new(),
            pending: HashMap::new(),
            next_seq: SEQ_BASE,
        }
    }

    fn sid(&self) -> &str {
        self.session.as_deref().unwrap_or("")
    }

    fn seq(&mut self, p: Pending) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        self.pending.insert(s, p);
        s
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some(text.into());
        self.error = error;
    }

    /// Whether a whole-diff request is in flight.
    #[must_use]
    pub const fn loading(&self) -> bool {
        self.loading
    }

    /// The picker's label for `b`.
    #[must_use]
    pub fn base_label(&self, b: Base) -> String {
        rules::base_label(b, self.branch_ref.as_deref())
    }

    /// The pending comments of the session shown.
    #[must_use]
    pub fn comments(&self) -> &[Comment] {
        self.comments.get(self.sid()).map_or(&[][..], Vec::as_slice)
    }

    /// A review goes to an agent, never a shell.
    #[must_use]
    pub fn can_review(&self) -> bool {
        self.kind != "shell"
    }

    /// The summary line: files, counts, live turn, from → to.
    #[must_use]
    pub fn summary(&self) -> String {
        let Some(d) = &self.diff else {
            return if self.loading { "Loading…".into() } else { String::new() };
        };
        let n = d.files.len() + d.files_omitted as usize;
        let mut s = format!("{n} file{} · +{} −{}", if n == 1 { "" } else { "s" }, d.added, d.deleted);
        if d.turn.as_ref().is_some_and(|t| t.live) {
            s.push_str(" · turn in progress");
        }
        if !d.from.label.is_empty() || !d.to.label.is_empty() {
            s.push_str(&format!(" · {} → {}", d.from.label, d.to.label));
        }
        s
    }

    /// The row the list should scroll to (and whether to the top), once.
    pub fn take_scroll(&mut self) -> Option<(usize, bool)> {
        self.scroll_to.take()
    }

    // ── session + requests ──────────────────────────────────────────────

    /// Show `id` (of `kind`): its chosen base, else Last turn for an agent
    /// and Uncommitted for a shell. `None` clears the tab.
    pub fn show(&mut self, session: Option<(&str, &str)>) {
        let Some((id, kind)) = session else {
            self.session = None;
            self.diff = None;
            self.reset_view();
            return;
        };
        if self.session.as_deref() == Some(id) {
            return;
        }
        self.session = Some(id.to_string());
        self.kind = kind.to_string();
        self.base = self.chosen.get(id).copied().unwrap_or_else(|| rules::default_base(kind));
        self.diff = None;
        self.status = None;
        self.reset_view();
        self.request();
    }

    fn reset_view(&mut self) {
        self.loading = false;
        self.again = false;
        self.fetching.clear();
        self.pending.clear();
        self.cursor = None;
        self.anchor = None;
        self.draft = None;
        self.rebuild();
    }

    /// Ask for the whole diff (coalesced: one in flight at a time, and a
    /// refetch queued behind it when something changed meanwhile).
    pub fn request(&mut self) {
        let Some(id) = self.session.clone() else { return };
        if self.loading {
            self.again = true;
            return;
        }
        self.loading = true;
        let seq = self.seq(Pending::Diff { path: None });
        self.out.send(frames::diff(&id, self.base, None, seq));
    }

    /// Ask for one file's hunks (a collapsed file expanded, or a page of a
    /// diff too big for one frame). Once per file until it lands.
    fn fetch(&mut self, path: &str) {
        let Some(id) = self.session.clone() else { return };
        if !self.fetching.insert(path.to_string()) {
            return;
        }
        let seq = self.seq(Pending::Diff { path: Some(path.into()) });
        self.out.send(frames::diff(&id, self.base, Some(path), seq));
    }

    /// The connection came back: whatever was in flight is lost.
    pub fn reconnected(&mut self) {
        self.loading = false;
        self.again = false;
        self.fetching.clear();
        self.pending.clear();
        if self.session.is_some() {
            self.request();
        }
    }

    /// Rows `range` are on screen: page in any not-yet-loaded file there.
    pub fn visible(&mut self, range: std::ops::Range<usize>) {
        let wanted: Vec<String> = self.rows[range.start.min(self.rows.len())..range.end.min(self.rows.len())]
            .iter()
            .filter_map(|r| match r {
                Row::Notice { file, fetch: true, .. } => self.page_path(*file),
                _ => None,
            })
            .collect();
        for p in wanted {
            self.fetch(&p);
        }
    }

    /// The path to page in for file `fi`, when it is expanded and its hunks
    /// were left out for size (collapsed noise waits for Show).
    fn page_path(&self, fi: usize) -> Option<String> {
        let f = self.diff.as_ref()?.files.get(fi)?;
        (f.hunks.is_empty() && f.hunks_omitted.is_some() && !self.is_collapsed(fi)).then(|| f.path.clone())
    }

    // ── engine events ───────────────────────────────────────────────────

    /// `flow.diff` arrived.
    pub fn received(&mut self, id: &str, base: Base, path: Option<&str>, payload: Payload) {
        if let Some(seq) = self
            .pending
            .iter()
            .find(|(_, p)| {
                **p == Pending::Diff {
                    path: path.map(str::to_string),
                }
            })
            .map(|(s, _)| *s)
        {
            self.pending.remove(&seq);
        }
        if Some(id) != self.session.as_deref() || base != self.base {
            return;
        }
        if base == Base::Branch {
            self.branch_ref = rules::branch_ref_from_label(&payload.from.label).map(str::to_string);
        }
        match path {
            Some(path) => {
                self.fetching.remove(path);
                let (Some(d), Some(f)) = (self.diff.as_mut(), payload.files.into_iter().next()) else {
                    return;
                };
                if let Some(slot) = d.files.iter_mut().find(|x| x.path == path) {
                    *slot = f;
                }
            }
            None => {
                self.loading = false;
                self.diff = Some(payload);
                // Pages asked of the old diff are void.
                self.fetching.clear();
                if std::mem::take(&mut self.again) {
                    self.request();
                }
            }
        }
        self.rebuild();
    }

    /// `flow.diff.result`: a hunk action or the review went through.
    pub fn result(&mut self, id: &str, action: &str, file: Option<&str>) {
        let kind = if action == "review" { Pending::Review } else { Pending::Action };
        if let Some(seq) = self.pending.iter().find(|(_, p)| **p == kind).map(|(s, _)| *s) {
            self.pending.remove(&seq);
        }
        if Some(id) != self.session.as_deref() {
            return;
        }
        let file = file.unwrap_or("the file");
        match action {
            "review" => {
                self.comments.remove(id);
                self.say("Review sent to the agent.", false);
                self.rebuild();
                return;
            }
            "revert" => self.say(format!("Reverted a hunk of {file}."), false),
            "stage" => self.say(format!("Staged a hunk of {file}."), false),
            "unstage" => self.say(format!("Unstaged a hunk of {file}."), false),
            _ => {}
        }
        self.request();
    }

    /// `flow.diff.changed`: refetch when it is the diff on screen.
    pub fn changed(&mut self, id: &str) {
        if Some(id) == self.session.as_deref() {
            self.request();
        }
    }

    /// A `flow.error` whose `ref` is `seq`. Returns whether it was ours.
    /// The engine's words are shown verbatim; nothing is retried or forced.
    /// A `stale` hunk refreshes the diff (the hunk changed under the user).
    pub fn failed(&mut self, seq: u64, code: Option<&str>, message: &str) -> bool {
        let Some(p) = self.pending.remove(&seq) else { return false };
        match p {
            Pending::Diff { path: None } => {
                self.loading = false;
                self.again = false;
                self.say(message, true);
            }
            Pending::Diff { path: Some(path) } => {
                self.fetching.remove(&path);
                self.say(message, true);
            }
            Pending::Action => {
                self.say(message, true);
                if code == Some("stale") || message.starts_with("stale:") {
                    self.request();
                }
            }
            Pending::Review => self.say(message, true),
        }
        true
    }

    // ── layout ──────────────────────────────────────────────────────────

    fn is_viewed(&self, f: &File) -> bool {
        self.viewed.get(self.sid()).is_some_and(|v| v.contains(&rules::viewed_key(&f.rule())))
    }

    /// File `fi`'s effective collapsed state.
    #[must_use]
    pub fn is_collapsed(&self, fi: usize) -> bool {
        let Some(f) = self.diff.as_ref().and_then(|d| d.files.get(fi)) else {
            return false;
        };
        if let Some(t) = self.toggles.get(self.sid()).and_then(|m| m.get(&f.path)) {
            return *t;
        }
        rules::display(&f.rule(), self.is_viewed(f)).collapsed
    }

    /// Whether file `fi` is marked viewed.
    #[must_use]
    pub fn file_viewed(&self, fi: usize) -> bool {
        self.diff.as_ref().and_then(|d| d.files.get(fi)).is_some_and(|f| self.is_viewed(f))
    }

    fn rebuild(&mut self) {
        let keep = self.cursor.and_then(|c| self.rows.get(c).cloned());
        if let Some(d) = &self.diff {
            let collapsed: Vec<bool> = (0..d.files.len()).map(|i| self.is_collapsed(i)).collect();
            let viewed: Vec<bool> = d.files.iter().map(|f| self.is_viewed(f)).collect();
            self.rows = rows(d, self.split, &collapsed, &viewed, self.comments());
            self.tree = d.tree();
        } else {
            self.rows = if self.session.is_none() {
                vec![Row::Banner("No session focused.".into())]
            } else {
                Vec::new()
            };
            self.tree = Vec::new();
        }
        self.cursor = keep
            .and_then(|k| self.rows.iter().position(|r| *r == k))
            .or_else(|| self.cursor.map(|c| c.min(self.rows.len().saturating_sub(1))));
        if self.rows.is_empty() {
            self.cursor = None;
        }
        self.anchor = None;
    }

    /// Whether row `r` is in the shift-selection.
    #[must_use]
    pub fn selected(&self, r: usize) -> bool {
        match (self.anchor, self.cursor) {
            (Some(a), Some(c)) => r >= a.min(c) && r <= a.max(c),
            _ => false,
        }
    }

    /// The file the cursor is in.
    #[must_use]
    pub fn cursor_file(&self) -> Option<usize> {
        self.cursor.and_then(|c| self.rows.get(c)).and_then(Row::file)
    }

    fn cursor_hunk(&self) -> Option<(usize, usize)> {
        let r = self.rows.get(self.cursor?)?;
        Some((r.file()?, r.hunk()?))
    }

    // ── moving ──────────────────────────────────────────────────────────

    /// Put the cursor on `row` (`extend` = shift: grow the selection).
    pub fn move_to(&mut self, row: usize, extend: bool) {
        if row >= self.rows.len() {
            return;
        }
        if extend {
            self.anchor = self.anchor.or(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = Some(row);
        self.scroll_to = Some((row, false));
    }

    fn next_line(&self) -> Option<usize> {
        let start = self.cursor.map_or(0, |c| c + 1);
        (start..self.rows.len()).find(|&i| self.rows[i].is_line())
    }

    fn previous_line(&self) -> Option<usize> {
        let end = self.cursor.unwrap_or(self.rows.len());
        (0..end).rev().find(|&i| self.rows[i].is_line())
    }

    fn move_hunk(&mut self, forward: bool) {
        let Some(d) = &self.diff else { return };
        let files: Vec<rules::File> = d.files.iter().map(File::rule).collect();
        let collapsed: Vec<bool> = (0..d.files.len()).map(|i| self.is_collapsed(i)).collect();
        let Some((f, h)) = rules::next_hunk(&files, &d.order(), &collapsed, self.cursor_hunk(), forward) else {
            return;
        };
        if let Some(row) = self.rows.iter().position(|r| *r == Row::HunkHeader { file: f, hunk: h }) {
            self.move_to(row, false);
        }
    }

    fn move_file(&mut self, forward: bool) {
        let Some(d) = &self.diff else { return };
        if let Some(next) = rules::next_file(&d.order(), self.cursor_file(), forward) {
            self.jump_to_file(next);
        }
    }

    /// Select file `fi` (the tree, `]`/`[`): its header goes to the top, and
    /// a file not loaded yet is paged in.
    pub fn jump_to_file(&mut self, fi: usize) {
        if let Some(row) = self.rows.iter().position(|r| *r == Row::FileHeader { file: fi }) {
            self.move_to(row, false);
            self.scroll_to = Some((row, true));
        }
        if let Some(p) = self.page_path(fi) {
            self.fetch(&p);
        }
    }

    // ── actions ─────────────────────────────────────────────────────────

    /// Pick a base (the picker). Remembered for this session.
    pub fn set_base(&mut self, b: Base) {
        let Some(id) = self.session.clone() else { return };
        if b == self.base {
            return;
        }
        self.chosen.insert(id, b);
        self.base = b;
        self.diff = None;
        self.status = None;
        self.reset_view();
        self.request();
    }

    pub fn toggle_split(&mut self) {
        self.split = !self.split;
        self.rebuild();
    }

    /// Show / hide file `fi`; showing one whose hunks were left out fetches them.
    pub fn toggle_collapse(&mut self, fi: usize) {
        let Some(path) = self.diff.as_ref().and_then(|d| d.files.get(fi)).map(|f| f.path.clone()) else {
            return;
        };
        let now = !self.is_collapsed(fi);
        self.toggles.entry(self.sid().to_string()).or_default().insert(path.clone(), now);
        self.rebuild();
        if let Some(p) = self.page_path(fi) {
            self.fetch(&p);
        }
    }

    /// Mark file `fi` viewed (it folds, and the cursor moves to the next
    /// file) or unviewed.
    pub fn toggle_viewed(&mut self, fi: usize) {
        let Some(f) = self.diff.as_ref().and_then(|d| d.files.get(fi)).cloned() else {
            return;
        };
        let sid = self.sid().to_string();
        let key = rules::viewed_key(&f.rule());
        if let Some(t) = self.toggles.get_mut(&sid) {
            t.remove(&f.path);
        }
        let set = self.viewed.entry(sid).or_default();
        if set.remove(&key) {
            self.rebuild();
            return;
        }
        set.insert(key);
        self.rebuild();
        let next = self.diff.as_ref().and_then(|d| rules::next_file(&d.order(), Some(fi), true));
        if let Some(next) = next {
            self.jump_to_file(next);
        }
    }

    /// The Revert confirmation for hunk `hi` of file `fi` (Cancel is the
    /// default; the caller shows it).
    #[must_use]
    pub fn ask_revert(&self, fi: usize, hi: usize) -> Option<RevertAsk> {
        let id = self.session.clone()?;
        let f = self.diff.as_ref()?.files.get(fi)?;
        let h = f.hunks.get(hi)?;
        Some(RevertAsk {
            session: id,
            base: self.base,
            hunk_id: h.id.clone(),
            title: format!("Revert this hunk of {}?", f.path),
            message: format!("{}\nThe worktree loses these lines. Other hunks are untouched.", h.header()),
        })
    }

    /// Revert, after the confirmation. Never forced: a `stale` refusal is shown.
    pub fn revert(&mut self, session: &str, base: Base, hunk_id: &str) {
        if Some(session) != self.session.as_deref() {
            return;
        }
        let seq = self.seq(Pending::Action);
        self.out.send(frames::diff_revert(session, base, hunk_id, seq));
    }

    /// Stage hunk `hi` of file `fi`, or unstage it when it is staged. The
    /// index is relative to HEAD, so only on the Uncommitted view.
    pub fn stage(&mut self, fi: usize, hi: usize) {
        if self.base != Base::Uncommitted {
            self.say("Stage works on the Uncommitted view (the index is relative to HEAD).", true);
            return;
        }
        let Some(id) = self.session.clone() else { return };
        let Some(h) = self.diff.as_ref().and_then(|d| d.files.get(fi)).and_then(|f| f.hunks.get(hi)) else {
            return;
        };
        let (hunk_id, unstage) = (h.id.clone(), h.staged);
        let seq = self.seq(Pending::Action);
        self.out.send(frames::diff_stage(&id, &hunk_id, unstage, seq));
    }

    /// Start a comment on the selected line or range (within one hunk), or
    /// on the file when the cursor is on its header.
    pub fn comment(&mut self) {
        let (Some(d), Some(c)) = (&self.diff, self.cursor) else { return };
        let Some(fi) = self.rows.get(c).and_then(Row::file) else { return };
        let f = &d.files[fi];
        let mut target = Comment {
            file: f.path.clone(),
            hunk_id: None,
            range: None,
            side: "new",
            text: String::new(),
        };
        if let Some(hi) = self.rows[c].hunk() {
            let h = &f.hunks[hi];
            target.hunk_id = Some(h.id.clone());
            let (lo, hi_row) = self.anchor.map_or((c, c), |a| (a.min(c), a.max(c)));
            let (mut news, mut olds) = (Vec::new(), Vec::new());
            for r in &self.rows[lo..=hi_row] {
                if r.file() != Some(fi) || r.hunk() != Some(hi) {
                    continue;
                }
                let li = match r {
                    Row::Line { line, .. } => Some(*line),
                    Row::Pair { left, right, .. } => right.filter(|&i| h.lines[i].new.is_some()).or(*left),
                    _ => None,
                };
                if let Some(l) = li.map(|i| &h.lines[i]) {
                    match (l.new, l.old) {
                        (Some(n), _) => news.push(n),
                        (None, Some(o)) => olds.push(o),
                        (None, None) => {}
                    }
                }
            }
            if let (Some(a), Some(b)) = (news.iter().min(), news.iter().max()) {
                target.range = Some((*a, *b));
            } else if let (Some(a), Some(b)) = (olds.iter().min(), olds.iter().max()) {
                target.range = Some((*a, *b));
                target.side = "old";
            }
        }
        self.draft = Some(Draft {
            field: Field::default(),
            target,
        });
    }

    /// Add the draft as a pending comment (a blank one is dropped).
    pub fn commit_draft(&mut self) {
        let Some(mut d) = self.draft.take() else { return };
        let text = d.field.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        d.target.text = text;
        self.comments.entry(self.sid().to_string()).or_default().push(d.target);
        self.rebuild();
    }

    pub fn remove_comment(&mut self, index: usize) {
        if let Some(list) = self.comments.get_mut(self.sid()) {
            if index < list.len() {
                list.remove(index);
            }
        }
        self.rebuild();
    }

    /// Send every pending comment as one `flow.diff.review`. They stay until
    /// the engine says it sent them; a `blocked` refusal is shown.
    pub fn submit_review(&mut self) {
        let Some(id) = self.session.clone() else { return };
        if !self.can_review() {
            self.say("A review goes to an agent, not a shell.", true);
            return;
        }
        let comments: Vec<serde_json::Value> = self.comments().iter().map(Comment::wire).collect();
        if comments.is_empty() {
            return;
        }
        let seq = self.seq(Pending::Review);
        self.say("Sending review…", false);
        self.out.send(frames::diff_review(&id, self.base, &comments, seq));
    }

    /// Click on row `row`: the cursor goes there (`shift` extends);
    /// `gutter` = the line-number gutter, which starts a comment.
    pub fn click(&mut self, row: usize, gutter: bool, shift: bool) {
        self.move_to(row, shift);
        self.scroll_to = None;
        if gutter && self.rows.get(row).is_some_and(Row::is_line) {
            self.comment();
        }
    }

    // ── keys ────────────────────────────────────────────────────────────

    /// A key while the Diff tab has focus: the draft's field first, then the
    /// viewer's bare keys (`smooth_flow_client::diff::KEYS`), arrows, Enter
    /// (show/hide the file) and Esc (drop the selection).
    pub fn key(&mut self, key: &str, key_char: Option<&str>, shift: bool, command: bool) -> KeyOutcome {
        if let Some(d) = &mut self.draft {
            match key {
                "enter" => self.commit_draft(),
                "escape" => self.draft = None,
                _ => {
                    let _: Edit = d.field.key(key, key_char, command);
                }
            }
            return KeyOutcome::Handled;
        }
        if command {
            return KeyOutcome::Ignored;
        }
        match key {
            "down" | "up" => {
                let to = if key == "down" { self.next_line() } else { self.previous_line() };
                if let Some(r) = to {
                    self.move_to(r, shift);
                }
                return KeyOutcome::Handled;
            }
            "enter" => {
                if let Some(fi) = self.cursor_file() {
                    self.toggle_collapse(fi);
                }
                return KeyOutcome::Handled;
            }
            "escape" => {
                self.anchor = None;
                return KeyOutcome::Handled;
            }
            _ => {}
        }
        let name = key_char.filter(|c| !c.is_empty()).unwrap_or(key);
        let Some(action) = rules::key_action(name) else { return KeyOutcome::Ignored };
        match action {
            "next_line" | "previous_line" => {
                let to = if action == "next_line" { self.next_line() } else { self.previous_line() };
                if let Some(r) = to {
                    self.move_to(r, false);
                }
            }
            "next_hunk" | "previous_hunk" => self.move_hunk(action == "next_hunk"),
            "next_file" | "previous_file" => self.move_file(action == "next_file"),
            "toggle_viewed" => {
                if let Some(fi) = self.cursor_file() {
                    self.toggle_viewed(fi);
                }
            }
            "comment" => self.comment(),
            "revert_hunk" => {
                if let Some(ask) = self.cursor_hunk().and_then(|(f, h)| self.ask_revert(f, h)) {
                    return KeyOutcome::Revert(ask);
                }
            }
            "stage_hunk" => {
                if let Some((f, h)) = self.cursor_hunk() {
                    self.stage(f, h);
                }
            }
            "toggle_split" => self.toggle_split(),
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn viewer() -> (Viewer, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (out, rx) = Outbox::channel();
        (Viewer::new(out), rx)
    }

    fn sent(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<Value> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect()
    }

    /// Two files: `src/a.rs` (two hunks, word + syntax spans) and a
    /// collapsed lockfile whose hunks were left out.
    fn payload(base: &str) -> Payload {
        serde_json::from_value(json!({
            "base": base,
            "from": {"ref": "abc", "label": "merge base with origin/main"},
            "to": {"ref": "", "label": "worktree"},
            "files": [
                {"path": "src/a.rs", "status": "modified", "added": 2, "deleted": 1, "language": "Rust", "hunks": [
                    {"id": "h1", "old_start": 1, "old_lines": 2, "new_start": 1, "new_lines": 2, "section": "fn main()", "lines": [
                        {"kind": "ctx", "old": 1, "new": 1, "text": "fn a() {"},
                        {"kind": "del", "old": 2, "text": "    let x = 1;", "words": [[12, 13]], "syntax": [[4, 7, 0]]},
                        {"kind": "add", "new": 2, "text": "    let x = 2;", "words": [[12, 13]], "syntax": [[4, 7, 0], [12, 13, 3]]}
                    ]},
                    {"id": "h2", "old_start": 9, "old_lines": 0, "new_start": 9, "new_lines": 1, "staged": true, "lines": [
                        {"kind": "add", "new": 9, "text": "// done", "no_eol": true}
                    ]}
                ]},
                {"path": "Cargo.lock", "status": "modified", "added": 40, "deleted": 3, "noise": "lockfile",
                 "collapsed_by_default": true, "hunks_omitted": "collapsed"}
            ],
            "added": 42, "deleted": 4,
            "legend": ["keyword","string","comment","number"]
        }))
        .expect("payload")
    }

    fn shown(base: Base) -> (Viewer, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (mut v, mut rx) = viewer();
        v.show(Some(("fs-1", "claude")));
        let req = sent(&mut rx);
        assert_eq!(req[0]["type"], "flow.diff");
        v.set_base(base);
        sent(&mut rx);
        let b = base_str(base);
        v.received("fs-1", base, None, payload(b));
        (v, rx)
    }

    #[test]
    fn opens_on_the_default_base_and_remembers_the_pick() {
        let (mut v, mut rx) = viewer();
        v.show(Some(("fs-1", "claude")));
        let r = sent(&mut rx);
        assert_eq!(
            (r[0]["base"].as_str(), r[0]["id"].as_str()),
            (Some("turn"), Some("fs-1")),
            "an agent opens on Last turn"
        );
        assert!(r[0]["seq"].as_u64().is_some_and(|s| s >= SEQ_BASE));
        assert!(r[0].get("path").is_none());
        v.set_base(Base::Branch);
        assert_eq!(sent(&mut rx)[0]["base"], "branch");
        v.show(Some(("fs-2", "shell")));
        assert_eq!(sent(&mut rx)[0]["base"], "uncommitted", "a shell opens on Uncommitted");
        v.show(Some(("fs-1", "claude")));
        assert_eq!(sent(&mut rx)[0]["base"], "branch", "the pick is remembered per session");
        v.show(Some(("fs-1", "claude")));
        assert!(sent(&mut rx).is_empty(), "the same session again asks nothing");
    }

    #[test]
    fn rows_follow_tree_order_and_collapse_noise() {
        let (v, _rx) = shown(Base::Turn);
        assert_eq!(
            v.rows,
            vec![
                Row::FileHeader { file: 0 },
                Row::HunkHeader { file: 0, hunk: 0 },
                Row::Line { file: 0, hunk: 0, line: 0 },
                Row::Line { file: 0, hunk: 0, line: 1 },
                Row::Line { file: 0, hunk: 0, line: 2 },
                Row::HunkHeader { file: 0, hunk: 1 },
                Row::Line { file: 0, hunk: 1, line: 0 },
                Row::FileHeader { file: 1 },
                Row::Notice {
                    file: 1,
                    text: "Lockfile · +40 −3 · collapsed".into(),
                    fetch: true
                },
            ],
            "directories before files: src/a.rs, then Cargo.lock"
        );
    }

    #[test]
    fn split_pairs_dels_with_adds() {
        let (mut v, _rx) = shown(Base::Turn);
        v.toggle_split();
        assert!(v.rows.contains(&Row::Pair {
            file: 0,
            hunk: 0,
            left: Some(1),
            right: Some(2)
        }));
        assert!(v.rows.contains(&Row::Pair {
            file: 0,
            hunk: 1,
            left: None,
            right: Some(0)
        }));
    }

    #[test]
    fn styled_lines_carry_syntax_and_word_spans() {
        let p = payload("turn");
        let add = &p.files[0].hunks[0].lines[2];
        let (text, runs) = styled(add, &p.legend);
        assert_eq!(text, "    let x = 2;");
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
        assert_eq!(
            runs[1],
            Run {
                len: 3,
                fg: palette::MAUVE,
                word: false
            },
            "`let` is a keyword"
        );
        assert!(
            runs.iter().any(|r| r.word && r.fg == palette::PEACH && r.len == 1),
            "the changed number: word span + number color"
        );
        let (t, r) = styled(&p.files[0].hunks[1].lines[0], &p.legend);
        assert!(t.ends_with("(no newline at end of file)"));
        assert_eq!(r.last().map(|x| x.fg), Some(palette::RED));
        // Context lines never get word backgrounds; spans past the end clamp.
        let ctx = Line {
            kind: LineKind::Ctx,
            old: None,
            new: None,
            text: "é".into(),
            no_eol: false,
            truncated: true,
            syntax: vec![[0, 99, 1]],
            words: vec![[0, 1]],
        };
        let (t, r) = styled(&ctx, &p.legend);
        assert_eq!(t, "é …");
        assert_eq!(
            r[0],
            Run {
                len: 2,
                fg: palette::GREEN,
                word: false
            }
        );
    }

    #[test]
    fn keys_navigate_lines_hunks_and_files() {
        let (mut v, _rx) = shown(Base::Turn);
        let press = |v: &mut Viewer, k: &str| v.key(k, Some(k), false, false);
        assert_eq!(press(&mut v, "n"), KeyOutcome::Handled);
        assert_eq!(v.cursor, Some(1), "first hunk of the first expanded file");
        press(&mut v, "j");
        assert_eq!(v.cursor, Some(2));
        press(&mut v, "n");
        assert_eq!(v.rows[v.cursor.unwrap_or(0)], Row::HunkHeader { file: 0, hunk: 1 });
        press(&mut v, "n");
        assert_eq!(
            v.rows[v.cursor.unwrap_or(0)],
            Row::HunkHeader { file: 0, hunk: 1 },
            "stops at the end (the lockfile is collapsed)"
        );
        press(&mut v, "]");
        assert_eq!(v.cursor, Some(7), "next file: the lockfile header");
        press(&mut v, "[");
        assert_eq!(v.cursor, Some(0));
        assert_eq!(v.take_scroll(), Some((0, true)), "the list scrolls its header to the top");
        assert_eq!(press(&mut v, "x"), KeyOutcome::Ignored);
        assert_eq!(v.key("j", Some("j"), false, true), KeyOutcome::Ignored, "chords are the keymap's");
    }

    #[test]
    fn expanding_a_collapsed_file_fetches_its_page_once() {
        let (mut v, mut rx) = shown(Base::Turn);
        v.toggle_collapse(1);
        let r = sent(&mut rx);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0]["path"].as_str(), r[0]["base"].as_str()), (Some("Cargo.lock"), Some("turn")));
        v.visible(0..v.rows.len());
        assert!(sent(&mut rx).is_empty(), "already fetching");
        let page: Payload = serde_json::from_value(json!({"base":"turn","files":[{"path":"Cargo.lock","status":"modified","added":40,"deleted":3,
            "noise":"lockfile","collapsed_by_default":true,"hunks":[{"id":"L1","lines":[{"kind":"add","new":1,"text":"x"}]}]}]}))
        .expect("page");
        v.received("fs-1", Base::Turn, Some("Cargo.lock"), page);
        assert!(v.rows.contains(&Row::Line { file: 1, hunk: 0, line: 0 }), "the page is spliced in");
    }

    #[test]
    fn budget_files_page_in_when_scrolled_into_view() {
        let (mut v, mut rx) = viewer();
        v.show(Some(("fs-1", "shell")));
        sent(&mut rx);
        let p: Payload = serde_json::from_value(json!({"base":"uncommitted","files":[
            {"path":"big.rs","status":"modified","added":900,"deleted":0,"hunks_omitted":"budget"}]}))
        .expect("payload");
        v.received("fs-1", Base::Uncommitted, None, p);
        assert_eq!(
            v.rows[1],
            Row::Notice {
                file: 0,
                text: "+900 −0 · loading…".into(),
                fetch: true
            }
        );
        v.visible(0..0);
        assert!(sent(&mut rx).is_empty(), "off screen: not yet");
        v.visible(0..5);
        let r = sent(&mut rx);
        assert_eq!((r.len(), r[0]["path"].as_str()), (1, Some("big.rs")));
    }

    #[test]
    fn viewed_folds_and_moves_on_and_a_changed_file_comes_back() {
        let (mut v, _rx) = shown(Base::Turn);
        v.move_to(1, false);
        v.key("v", Some("v"), false, false);
        assert!(v.is_collapsed(0) && v.file_viewed(0));
        assert!(v.rows.contains(&Row::Notice {
            file: 0,
            text: "Viewed".into(),
            fetch: false
        }));
        // The agent touches the file again: new hunk ids, so it's unviewed.
        let mut p = payload("turn");
        p.files[0].hunks[0].id = "h1b".into();
        v.received("fs-1", Base::Turn, None, p);
        assert!(!v.file_viewed(0) && !v.is_collapsed(0));
    }

    #[test]
    fn revert_asks_and_stage_needs_uncommitted() {
        let (mut v, mut rx) = shown(Base::Turn);
        v.move_to(4, false);
        let KeyOutcome::Revert(ask) = v.key("r", Some("r"), false, false) else {
            panic!("r asks")
        };
        assert_eq!(ask.hunk_id, "h1");
        assert!(ask.title.contains("src/a.rs") && ask.message.starts_with("@@ -1,2 +1,2 @@ fn main()"));
        assert!(sent(&mut rx).is_empty(), "nothing is sent before the confirmation");
        v.revert(&ask.session, ask.base, &ask.hunk_id);
        let r = sent(&mut rx);
        assert_eq!(
            (r[0]["type"].as_str(), r[0]["hunk_id"].as_str(), r[0]["base"].as_str()),
            (Some("flow.diff.revert"), Some("h1"), Some("turn"))
        );
        v.key("s", Some("s"), false, false);
        assert!(sent(&mut rx).is_empty());
        assert!(v.status.as_deref().is_some_and(|s| s.contains("Uncommitted")));

        let (mut v, mut rx) = shown(Base::Uncommitted);
        v.move_to(6, false);
        v.key("s", Some("s"), false, false);
        let r = sent(&mut rx);
        assert_eq!(
            (r[0]["type"].as_str(), r[0]["hunk_id"].as_str()),
            (Some("flow.diff.unstage"), Some("h2")),
            "a staged hunk unstages"
        );
    }

    #[test]
    fn stale_is_shown_verbatim_and_refreshes_without_retrying() {
        let (mut v, mut rx) = shown(Base::Turn);
        v.revert("fs-1", Base::Turn, "h1");
        let seq = sent(&mut rx)[0]["seq"].as_u64().expect("seq");
        assert!(!v.failed(seq + 999, Some("stale"), "x"), "not ours");
        let msg = "stale: hunk h1 is not in the current turn diff — refresh";
        assert!(v.failed(seq, Some("stale"), msg));
        assert_eq!((v.status.as_deref(), v.error), (Some(msg), true));
        let r = sent(&mut rx);
        assert_eq!(r.len(), 1, "one refresh, no retried revert");
        assert_eq!(r[0]["type"], "flow.diff");
    }

    #[test]
    fn refreshes_coalesce_while_one_is_in_flight() {
        let (mut v, mut rx) = viewer();
        v.show(Some(("fs-1", "claude")));
        assert_eq!(sent(&mut rx).len(), 1);
        v.changed("fs-1");
        v.changed("fs-1");
        v.changed("other");
        assert!(sent(&mut rx).is_empty(), "queued behind the request in flight");
        v.received("fs-1", Base::Turn, None, payload("turn"));
        assert_eq!(sent(&mut rx).len(), 1, "one refetch for the changes meanwhile");
        v.received("fs-1", Base::Turn, None, payload("turn"));
        v.received("fs-1", Base::Branch, None, payload("branch"));
        assert!(v.diff.as_ref().is_some_and(|d| d.base == Base::Turn), "another base's reply is dropped");
        assert!(sent(&mut rx).is_empty());
        v.reconnected();
        assert_eq!(sent(&mut rx).len(), 1, "a reconnect refetches");
    }

    #[test]
    fn comments_batch_into_one_review_and_blocked_keeps_them() {
        let (mut v, mut rx) = shown(Base::Turn);
        // Shift-select the del and the add of hunk 1: new-side line 2.
        v.click(3, false, false);
        v.click(4, true, true);
        let d = v.draft.as_ref().expect("gutter click starts a comment");
        assert_eq!((d.target.range, d.target.side, d.target.place()), (Some((2, 2)), "new", "src/a.rs:2".into()));
        for k in ["w", "h", "y"] {
            v.key(k, Some(k), false, false);
        }
        v.key("space", None, false, false);
        v.key("enter", None, false, false);
        assert!(v.draft.is_none());
        assert_eq!(v.comments()[0].text, "why");
        assert!(v.rows.contains(&Row::Comment { file: 0, index: 0 }), "shown under its line");
        // A deleted-only selection comments on the old side.
        v.move_to(3, false);
        v.comment();
        assert_eq!(v.draft.as_ref().map(|d| (d.target.range, d.target.side)), Some((Some((2, 2)), "old")));
        v.key("escape", None, false, false);
        assert!(v.draft.is_none() && v.comments().len() == 1, "Esc drops the draft");
        // A file-level comment.
        v.move_to(0, false);
        v.comment();
        v.key("k", Some("k"), false, false);
        v.key("enter", None, false, false);
        assert_eq!(v.comments()[1].range, None);

        v.submit_review();
        let r = sent(&mut rx);
        assert_eq!(r[0]["type"], "flow.diff.review");
        assert_eq!(r[0]["comments"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            r[0]["comments"][0],
            json!({"file":"src/a.rs","hunk_id":"h1","line_range":[2,2],"side":"new","text":"why"})
        );
        let seq = r[0]["seq"].as_u64().expect("seq");
        v.failed(seq, Some("blocked"), "blocked: fs-1 is needs_you — it cannot take a review now");
        assert_eq!(v.comments().len(), 2, "a refused review keeps its comments");
        assert!(v.status.as_deref().is_some_and(|s| s.starts_with("blocked:")));
        assert!(sent(&mut rx).is_empty(), "never retried");
        v.submit_review();
        sent(&mut rx);
        v.result("fs-1", "review", None);
        assert!(v.comments().is_empty());
        assert_eq!(v.status.as_deref(), Some("Review sent to the agent."));
    }

    #[test]
    fn shells_get_no_review_and_results_refresh() {
        let (mut v, mut rx) = viewer();
        v.show(Some(("fs-1", "shell")));
        sent(&mut rx);
        v.received("fs-1", Base::Uncommitted, None, payload("uncommitted"));
        assert!(!v.can_review());
        v.move_to(4, false);
        v.comment();
        v.key("a", Some("a"), false, false);
        v.key("enter", None, false, false);
        v.submit_review();
        assert!(sent(&mut rx).is_empty());
        assert!(v.error);
        v.result("fs-1", "stage", Some("src/a.rs"));
        assert_eq!(v.status.as_deref(), Some("Staged a hunk of src/a.rs."));
        assert_eq!(sent(&mut rx)[0]["type"], "flow.diff", "a hunk action refreshes");
        assert!(v.summary().starts_with("2 files · +42 −4"));
        v.received("fs-1", Base::Uncommitted, None, payload("uncommitted"));
        v.set_base(Base::Branch);
        sent(&mut rx);
        v.received("fs-1", Base::Branch, None, payload("branch"));
        assert_eq!(v.base_label(Base::Branch), "vs main");
    }
}
