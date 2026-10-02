//! The wire model of a structured diff (th-26f5b9). Serialized as-is inside
//! `flow.diff` and `GET /api/flow/sessions/{id}/diff`; every client renders
//! from it and never runs `git` itself.
//!
//! Offsets inside a line (`syntax`, `words`) are **Unicode scalar** offsets
//! (`char`s in Rust, `unicodeScalars` in Swift, code points in Kotlin),
//! half-open `[start, end)`. Empty arrays and `false` flags are omitted from
//! the wire to keep a large diff small.

use serde::{Deserialize, Serialize};

/// What a diff is taken against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffBase {
    /// What the agent's last turn changed (turn snapshots, see `snapshot`).
    Turn,
    /// The worktree (untracked files included) against `HEAD`.
    Uncommitted,
    /// The worktree against the merge base with the default branch.
    Branch,
}

impl DiffBase {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Uncommitted => "uncommitted",
            Self::Branch => "branch",
        }
    }
}

impl std::str::FromStr for DiffBase {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.trim() {
            "turn" => Ok(Self::Turn),
            "uncommitted" => Ok(Self::Uncommitted),
            "branch" => Ok(Self::Branch),
            other => Err(anyhow::anyhow!("unknown diff base `{other}` (turn | uncommitted | branch)")),
        }
    }
}

/// One side of the comparison, for the header a client shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSide {
    /// A tree or commit id git understands.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// Human words: `HEAD`, `merge base with origin/main`, `turn start`, `worktree`.
    pub label: String,
}

/// Which turn a `turn` diff shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnInfo {
    /// The snapshot sequence number of the turn's end (or of its start when live).
    pub seq: i64,
    /// The turn is still running: the right side is the worktree now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub live: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

/// A whole diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    pub base: DiffBase,
    pub from: DiffSide,
    pub to: DiffSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnInfo>,
    /// Why the diff is empty or partial, in words (no turn yet, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub files: Vec<DiffFile>,
    /// Total added / deleted lines across every file, truncated or not.
    pub added: u32,
    pub deleted: u32,
    /// Files past [`super::MAX_FILES`] or past the patch byte cap are not
    /// listed at all; this says so, and how many.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub files_omitted: u32,
    /// `syntax` span kinds by index (always [`TokenKind::NAMES`]), so the
    /// wire is self-describing.
    pub legend: Vec<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
const fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// How a file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    /// Only the mode changed (`chmod +x`).
    ModeChanged,
}

/// Why a file starts collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Noise {
    Lockfile,
    Generated,
    Vendored,
    Minified,
    /// More than [`super::LARGE_FILE_LINES`] changed lines.
    Large,
}

/// Why a file's hunks are not in this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Omitted {
    /// Noise: collapsed by default; fetch it with `path` when the user asks.
    Collapsed,
    /// The frame's byte budget ran out; fetch it with `path`.
    Budget,
}

/// One file of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    /// The new path (the old one for a deletion).
    pub path: String,
    /// The old path of a rename or copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: FileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_mode: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
    /// The syntax the engine highlighted with (`Rust`, `TypeScript`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub added: u32,
    pub deleted: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noise: Option<Noise>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub collapsed_by_default: bool,
    /// The hunks were left out of this frame; `hunks` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunks_omitted: Option<Omitted>,
    /// Some hunks or lines were cut ([`super::MAX_FILE_LINES`] or the byte budget).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<Hunk>,
}

/// One hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// Stable across recomputes while the hunk's content is unchanged —
    /// independent of line numbers, so reverting one hunk leaves the ids of
    /// the others alone. What `flow.diff.revert|stage|unstage` name.
    pub id: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The text after the second `@@` (git's function context), if any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub section: String,
    pub lines: Vec<Line>,
    /// Lines past [`super::MAX_HUNK_LINES`] were cut.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// `uncommitted` only: the same hunk is already in the index.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staged: bool,
}

/// A line's role in a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Add,
    Del,
    Ctx,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub kind: LineKind,
    /// 1-based old-side number (`del` and `ctx`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<u32>,
    /// 1-based new-side number (`add` and `ctx`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<u32>,
    /// The content, without the `+`/`-`/` ` prefix or the line ending.
    pub text: String,
    /// `\ No newline at end of file` follows this line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_eol: bool,
    /// The line ended in `\r\n` (stripped from `text`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cr: bool,
    /// `text` was cut at [`super::MAX_LINE_CHARS`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Syntax spans `[start, end, kind]` — `kind` indexes [`Diff::legend`].
    /// Uncolored text has no span.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub syntax: Vec<[u32; 3]>,
    /// Word-level change spans `[start, end)` against the paired line of the
    /// opposite kind (a `del` paired with an `add` in the same change block).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<[u32; 2]>,
}

/// Syntax token kinds a client maps to its theme. The index is the wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TokenKind {
    Keyword = 0,
    String = 1,
    Comment = 2,
    Number = 3,
    Constant = 4,
    Function = 5,
    Type = 6,
    Variable = 7,
    Property = 8,
    Operator = 9,
    Punctuation = 10,
    Tag = 11,
    Attribute = 12,
    Macro = 13,
    Escape = 14,
    Heading = 15,
    Link = 16,
}

impl TokenKind {
    /// Names by index — `Diff::legend`.
    pub const NAMES: [&'static str; 17] = [
        "keyword",
        "string",
        "comment",
        "number",
        "constant",
        "function",
        "type",
        "variable",
        "property",
        "operator",
        "punctuation",
        "tag",
        "attribute",
        "macro",
        "escape",
        "heading",
        "link",
    ];

    /// The legend as owned strings.
    #[must_use]
    pub fn legend() -> Vec<String> {
        Self::NAMES.iter().map(|s| (*s).to_string()).collect()
    }
}

/// One review comment (`flow.diff.review`): a line range of one hunk and
/// what to say about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    pub file: String,
    #[serde(default)]
    pub hunk_id: Option<String>,
    /// Inclusive `[first, last]` line numbers on `side`.
    #[serde(default)]
    pub line_range: Option<[u32; 2]>,
    /// Which numbering `line_range` uses: `new` (default) or `old` (a deleted line).
    #[serde(default)]
    pub side: Option<String>,
    pub text: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn base_parses_and_round_trips() {
        for b in [DiffBase::Turn, DiffBase::Uncommitted, DiffBase::Branch] {
            assert_eq!(b.as_str().parse::<DiffBase>().unwrap(), b);
            assert_eq!(serde_json::to_value(b).unwrap(), b.as_str());
        }
        assert!("main".parse::<DiffBase>().is_err());
    }

    #[test]
    fn empty_fields_stay_off_the_wire() {
        let l = Line {
            kind: LineKind::Ctx,
            old: Some(1),
            new: Some(1),
            text: "x".into(),
            no_eol: false,
            cr: false,
            truncated: false,
            syntax: vec![],
            words: vec![],
        };
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v, serde_json::json!({"kind":"ctx","old":1,"new":1,"text":"x"}));
        let back: Line = serde_json::from_value(v).unwrap();
        assert_eq!(back, l);
        assert_eq!(TokenKind::legend().len(), 17);
        assert_eq!(TokenKind::NAMES[TokenKind::Link as usize], "link");
    }
}
