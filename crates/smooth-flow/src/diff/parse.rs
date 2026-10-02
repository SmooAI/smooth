//! Parse `git diff` unified output into raw files and hunks, and write one
//! hunk back out as a patch `git apply` accepts.
//!
//! The raw form keeps every byte the patch needs (the `\r` of a CRLF line,
//! the `\ No newline` marker); the display model in `build` is derived from
//! it and may truncate, the raw form never does.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use super::model::{FileStatus, LineKind};

/// One line of a hunk, prefix stripped, `\r` kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLine {
    pub kind: LineKind,
    pub text: String,
    pub no_eol: bool,
}

/// One `@@` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub section: String,
    pub lines: Vec<RawLine>,
}

/// One file of the patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    /// `None` for `/dev/null` (an added file).
    pub old_path: Option<String>,
    /// `None` for `/dev/null` (a deleted file).
    pub new_path: Option<String>,
    pub status: FileStatus,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub binary: bool,
    pub hunks: Vec<RawHunk>,
}

impl RawFile {
    /// The path a client shows: the new one, or the old one for a deletion.
    #[must_use]
    pub fn path(&self) -> &str {
        self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or("")
    }

    /// `(added, deleted)` line counts.
    #[must_use]
    pub fn counts(&self) -> (u32, u32) {
        let mut a = 0u32;
        let mut d = 0u32;
        for l in self.hunks.iter().flat_map(|h| &h.lines) {
            match l.kind {
                LineKind::Add => a = a.saturating_add(1),
                LineKind::Del => d = d.saturating_add(1),
                LineKind::Ctx => {}
            }
        }
        (a, d)
    }

    /// Stable hunk ids, in hunk order (see [`hunk_id`]).
    #[must_use]
    pub fn hunk_ids(&self) -> Vec<String> {
        let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        self.hunks
            .iter()
            .map(|h| {
                let base = hunk_id(self.path(), h);
                let n = seen.entry(base.clone()).or_insert(0);
                *n += 1;
                if *n == 1 {
                    base
                } else {
                    // Two byte-identical hunks in one file: number the repeats.
                    let mut hasher = Sha256::new();
                    hasher.update(base.as_bytes());
                    hasher.update(n.to_le_bytes());
                    hex16(&hasher.finalize())
                }
            })
            .collect()
    }
}

fn hex16(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(16);
    for b in bytes.iter().take(8) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A hunk's id: the path and the lines, NOT the line numbers — so reverting
/// one hunk does not change the ids of the hunks below it.
#[must_use]
pub fn hunk_id(path: &str, h: &RawHunk) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hasher.update([0u8]);
    for l in &h.lines {
        hasher.update(match l.kind {
            LineKind::Add => b"+",
            LineKind::Del => b"-",
            LineKind::Ctx => b" ",
        });
        hasher.update(l.text.as_bytes());
        if l.no_eol {
            hasher.update(b"\\");
        }
        hasher.update(b"\n");
    }
    hex16(&hasher.finalize())
}

/// Undo git's C-style quoting (`"a/sp\303\251c\"ial"`) — octal escapes are
/// bytes, so the result is decoded as UTF-8 (lossy).
#[must_use]
pub fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'\\' || i + 1 >= bytes.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let e = bytes[i + 1];
        i += 2;
        match e {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = u32::from(e - b'0');
                let mut taken = 1;
                while taken < 3 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                    v = v * 8 + u32::from(bytes[i] - b'0');
                    i += 1;
                    taken += 1;
                }
                out.push(u8::try_from(v & 0xff).unwrap_or(0));
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Quote a path the way git does when it has to (`"`, `\`, control bytes).
#[must_use]
pub fn quote(path: &str) -> String {
    if !path.chars().any(|c| c == '"' || c == '\\' || c.is_control()) {
        return path.to_string();
    }
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    let _ = write!(out, "\\{b:03o}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A `---`/`+++` name: `/dev/null` ⇒ None; else unquoted with the `a/`/`b/`
/// prefix and git's trailing tab (added after a name with a space) removed.
fn side_name(raw: &str, prefix: &str) -> Option<String> {
    let raw = raw.strip_suffix('\t').unwrap_or(raw);
    if raw == "/dev/null" {
        return None;
    }
    let name = unquote(raw);
    Some(name.strip_prefix(prefix).map_or_else(|| name.clone(), str::to_string))
}

/// Paths from `diff --git a/X b/Y` — only used when the patch has no
/// `---`/`+++` (binary, mode-only, empty file).
fn git_line_paths(rest: &str) -> Option<(String, String)> {
    if rest.starts_with('"') {
        // Quoted: two C-strings (or a quoted and a bare one).
        let end = find_quote_end(rest)?;
        let a = unquote(&rest[..=end]);
        let b_raw = rest[end + 1..].trim_start();
        let b = if b_raw.starts_with('"') { unquote(b_raw) } else { b_raw.to_string() };
        return Some((a.strip_prefix("a/")?.to_string(), b.strip_prefix("b/")?.to_string()));
    }
    // Bare and equal (no rename — a rename always has `rename from/to`):
    // `a/P b/P` is 2·len(P) + 5 bytes.
    let n = rest.len();
    if n >= 5 && n % 2 == 1 {
        let half = (n - 1) / 2;
        let (a, b) = (&rest[..half], &rest[half + 1..]);
        if let (Some(pa), Some(pb)) = (a.strip_prefix("a/"), b.strip_prefix("b/")) {
            if pa == pb {
                return Some((pa.to_string(), pb.to_string()));
            }
        }
    }
    // Last resort: split at " b/".
    let i = rest.find(" b/")?;
    Some((rest[..i].strip_prefix("a/")?.to_string(), rest[i + 3..].to_string()))
}

fn find_quote_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// `@@ -a[,b] +c[,d] @@ section`
fn parse_hunk_header(line: &str) -> Option<RawHunk> {
    let rest = line.strip_prefix("@@ -")?;
    let close = rest.find(" @@")?;
    let ranges = &rest[..close];
    let section = rest[close + 3..].trim_start().to_string();
    let (old, new) = ranges.split_once(" +")?;
    let range = |r: &str| -> Option<(u32, u32)> {
        match r.split_once(',') {
            Some((s, n)) => Some((s.parse().ok()?, n.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (old_start, old_lines) = range(old)?;
    let (new_start, new_lines) = range(new)?;
    Some(RawHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section,
        lines: Vec::new(),
    })
}

#[derive(Default)]
struct Building {
    file: Option<RawFile>,
    git_paths: Option<(String, String)>,
    saw_minus: bool,
    saw_plus: bool,
    /// Old/new lines still owed to the current hunk.
    owed: (u32, u32),
}

impl Building {
    fn finish(&mut self, out: &mut Vec<RawFile>) {
        let Some(mut f) = self.file.take() else { return };
        if !self.saw_minus && !self.saw_plus {
            if let Some((a, b)) = self.git_paths.take() {
                if f.old_path.is_none() && f.status != FileStatus::Added {
                    f.old_path = Some(a);
                }
                if f.new_path.is_none() && f.status != FileStatus::Deleted {
                    f.new_path = Some(b);
                }
            }
        }
        if f.status == FileStatus::Modified && f.hunks.is_empty() && !f.binary && f.old_mode.is_some() && f.new_mode.is_some() {
            f.status = FileStatus::ModeChanged;
        }
        out.push(f);
        *self = Self::default();
    }
}

/// Parse a whole `git diff` output (run with `--src-prefix=a/
/// --dst-prefix=b/`, no color, no external diff).
#[must_use]
#[allow(clippy::too_many_lines, reason = "one pass over the patch grammar; splitting it scatters the state")]
pub fn parse(patch: &str) -> Vec<RawFile> {
    let mut out = Vec::new();
    let mut b = Building::default();
    for line in patch.split('\n') {
        // Inside a hunk that still owes lines, every line is content.
        if b.owed != (0, 0) {
            if let Some(f) = b.file.as_mut() {
                if let Some(h) = f.hunks.last_mut() {
                    let (kind, text) = match line.as_bytes().first() {
                        Some(b'+') => (LineKind::Add, &line[1..]),
                        Some(b'-') => (LineKind::Del, &line[1..]),
                        Some(b' ') => (LineKind::Ctx, &line[1..]),
                        // An empty context line some tools emit without its space.
                        None => (LineKind::Ctx, ""),
                        Some(b'\\') => {
                            if let Some(last) = h.lines.last_mut() {
                                last.no_eol = true;
                            }
                            continue;
                        }
                        Some(_) => {
                            // Malformed: stop owing and fall through.
                            b.owed = (0, 0);
                            continue;
                        }
                    };
                    match kind {
                        LineKind::Add => b.owed.1 = b.owed.1.saturating_sub(1),
                        LineKind::Del => b.owed.0 = b.owed.0.saturating_sub(1),
                        LineKind::Ctx => {
                            b.owed.0 = b.owed.0.saturating_sub(1);
                            b.owed.1 = b.owed.1.saturating_sub(1);
                        }
                    }
                    h.lines.push(RawLine {
                        kind,
                        text: text.to_string(),
                        no_eol: false,
                    });
                    continue;
                }
            }
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            b.finish(&mut out);
            b.git_paths = git_line_paths(rest);
            b.file = Some(RawFile {
                old_path: None,
                new_path: None,
                status: FileStatus::Modified,
                old_mode: None,
                new_mode: None,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(f) = b.file.as_mut() else { continue };
        if line.starts_with('\\') {
            // `\ No newline at end of file` right after a hunk's last line.
            if let Some(last) = f.hunks.last_mut().and_then(|h| h.lines.last_mut()) {
                last.no_eol = true;
            }
            continue;
        }
        if line.starts_with("@@ ") {
            if let Some(h) = parse_hunk_header(line) {
                b.owed = (h.old_lines, h.new_lines);
                f.hunks.push(h);
            }
            continue;
        }
        if !f.hunks.is_empty() {
            continue;
        }
        // Extended header.
        if let Some(m) = line.strip_prefix("old mode ") {
            f.old_mode = Some(m.trim().to_string());
        } else if let Some(m) = line.strip_prefix("new mode ") {
            f.new_mode = Some(m.trim().to_string());
        } else if let Some(m) = line.strip_prefix("deleted file mode ") {
            f.status = FileStatus::Deleted;
            f.old_mode = Some(m.trim().to_string());
        } else if let Some(m) = line.strip_prefix("new file mode ") {
            f.status = FileStatus::Added;
            f.new_mode = Some(m.trim().to_string());
        } else if let Some(p) = line.strip_prefix("rename from ") {
            f.status = FileStatus::Renamed;
            f.old_path = Some(unquote(p));
        } else if let Some(p) = line.strip_prefix("rename to ") {
            f.status = FileStatus::Renamed;
            f.new_path = Some(unquote(p));
        } else if let Some(p) = line.strip_prefix("copy from ") {
            f.status = FileStatus::Copied;
            f.old_path = Some(unquote(p));
        } else if let Some(p) = line.strip_prefix("copy to ") {
            f.status = FileStatus::Copied;
            f.new_path = Some(unquote(p));
        } else if let Some(p) = line.strip_prefix("--- ") {
            b.saw_minus = true;
            f.old_path = side_name(p, "a/");
        } else if let Some(p) = line.strip_prefix("+++ ") {
            b.saw_plus = true;
            f.new_path = side_name(p, "b/");
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            f.binary = true;
        } else if let Some(idx) = line.strip_prefix("index ") {
            // `index abc..def 100644` — the mode of an unchanged-mode file.
            if let Some((_, mode)) = idx.split_once(' ') {
                let mode = mode.trim().to_string();
                if f.old_mode.is_none() && f.status != FileStatus::Added {
                    f.old_mode = Some(mode.clone());
                }
                if f.new_mode.is_none() && f.status != FileStatus::Deleted {
                    f.new_mode = Some(mode);
                }
            }
        }
    }
    b.finish(&mut out);
    // An unchanged-mode `index` line set both modes equal: that is not a mode change.
    for f in &mut out {
        if f.old_mode == f.new_mode && f.status != FileStatus::ModeChanged {
            if f.status == FileStatus::Modified || f.status == FileStatus::Renamed || f.status == FileStatus::Copied {
                f.old_mode = None;
                f.new_mode = None;
            }
        } else if f.status == FileStatus::Added {
            f.old_mode = None;
        } else if f.status == FileStatus::Deleted {
            f.new_mode = None;
        }
    }
    out
}

/// A patch carrying exactly one hunk of `file`, for `git apply`. Content
/// only: a rename is written against the new path (the file the worktree
/// has), and a mode change is left out — neither is a hunk. An added or
/// deleted file keeps its `new file`/`deleted file` line so applying (or
/// reversing) the whole-file hunk creates or removes the file.
#[must_use]
pub fn hunk_patch(file: &RawFile, hunk: &RawHunk) -> String {
    let path = file.path();
    let q = |prefix: &str| {
        let p = quote(&format!("{prefix}{path}"));
        if p.contains(' ') && !p.starts_with('"') {
            format!("{p}\t")
        } else {
            p
        }
    };
    let mut out = String::new();
    let _ = writeln!(out, "diff --git {} {}", quote(&format!("a/{path}")), quote(&format!("b/{path}")));
    match file.status {
        FileStatus::Added => {
            let _ = writeln!(out, "new file mode {}", file.new_mode.as_deref().unwrap_or("100644"));
            let _ = writeln!(out, "--- /dev/null");
            let _ = writeln!(out, "+++ {}", q("b/"));
        }
        FileStatus::Deleted => {
            let _ = writeln!(out, "deleted file mode {}", file.old_mode.as_deref().unwrap_or("100644"));
            let _ = writeln!(out, "--- {}", q("a/"));
            let _ = writeln!(out, "+++ /dev/null");
        }
        _ => {
            let _ = writeln!(out, "--- {}", q("a/"));
            let _ = writeln!(out, "+++ {}", q("b/"));
        }
    }
    let range = |start: u32, n: u32| if n == 1 { format!("{start}") } else { format!("{start},{n}") };
    let _ = writeln!(
        out,
        "@@ -{} +{} @@{}",
        range(hunk.old_start, hunk.old_lines),
        range(hunk.new_start, hunk.new_lines),
        if hunk.section.is_empty() { String::new() } else { format!(" {}", hunk.section) }
    );
    for l in &hunk.lines {
        let p = match l.kind {
            LineKind::Add => '+',
            LineKind::Del => '-',
            LineKind::Ctx => ' ',
        };
        let _ = writeln!(out, "{p}{}", l.text);
        if l.no_eol {
            out.push_str("\\ No newline at end of file\n");
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    const MODIFY: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@ fn main() {
 one
-two
+TWO
+three
 four
@@ -10 +11 @@
-x
\\ No newline at end of file
+y
\\ No newline at end of file
";

    #[test]
    fn parses_a_modified_file_with_two_hunks_and_no_eol() {
        let files = parse(MODIFY);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path(), "src/lib.rs");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!((f.old_mode.clone(), f.new_mode.clone()), (None, None), "unchanged mode is not a mode change");
        assert_eq!(f.hunks.len(), 2);
        let h = &f.hunks[0];
        assert_eq!((h.old_start, h.old_lines, h.new_start, h.new_lines), (1, 3, 1, 4));
        assert_eq!(h.section, "fn main() {");
        assert_eq!(h.lines.len(), 5);
        assert_eq!(h.lines[1], RawLine { kind: LineKind::Del, text: "two".into(), no_eol: false });
        let h2 = &f.hunks[1];
        assert_eq!((h2.old_start, h2.old_lines, h2.new_start, h2.new_lines), (10, 1, 11, 1));
        assert!(h2.lines[0].no_eol && h2.lines[1].no_eol);
        assert_eq!(f.counts(), (3, 2));
    }

    #[test]
    fn hunk_patch_round_trips_through_the_parser() {
        let files = parse(MODIFY);
        let f = &files[0];
        for h in &f.hunks {
            let p = hunk_patch(f, h);
            let back = parse(&p);
            assert_eq!(back.len(), 1, "{p}");
            assert_eq!(back[0].hunks, vec![h.clone()], "{p}");
            assert_eq!(hunk_id(f.path(), &back[0].hunks[0]), hunk_id(f.path(), h));
        }
    }

    #[test]
    fn ids_ignore_line_numbers_but_not_content() {
        let f = &parse(MODIFY)[0];
        let mut moved = f.hunks[0].clone();
        moved.old_start += 7;
        moved.new_start += 7;
        assert_eq!(hunk_id("src/lib.rs", &moved), hunk_id("src/lib.rs", &f.hunks[0]));
        let mut edited = f.hunks[0].clone();
        edited.lines[2].text.push('!');
        assert_ne!(hunk_id("src/lib.rs", &edited), hunk_id("src/lib.rs", &f.hunks[0]));
        assert_ne!(hunk_id("other.rs", &f.hunks[0]), hunk_id("src/lib.rs", &f.hunks[0]));
        let ids = f.hunk_ids();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[0].len(), 16);
    }

    #[test]
    fn duplicate_hunks_get_distinct_ids() {
        let mut f = parse(MODIFY).remove(0);
        f.hunks[1] = f.hunks[0].clone();
        let ids = f.hunk_ids();
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn added_deleted_renamed_binary_and_mode_only() {
        let patch = "diff --git a/new.txt b/new.txt
new file mode 100755
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+a
+b
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.rs b/new name.rs
similarity index 90%
rename from old name.rs
rename to new name.rs
index 5555555..6666666 100644
--- a/old name.rs\t
+++ b/new name.rs\t
@@ -1 +1 @@
-fn a() {}
+fn b() {}
diff --git a/logo.png b/logo.png
index 7777777..8888888 100644
Binary files a/logo.png and b/logo.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
diff --git a/moved.txt b/elsewhere.txt
similarity index 100%
rename from moved.txt
rename to elsewhere.txt
";
        let files = parse(patch);
        assert_eq!(files.len(), 6);
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[0].old_path, None);
        assert_eq!(files[0].new_mode.as_deref(), Some("100755"));
        assert_eq!(files[0].counts(), (2, 0));
        assert_eq!(files[1].status, FileStatus::Deleted);
        assert_eq!(files[1].path(), "gone.txt");
        assert_eq!(files[1].new_path, None);
        assert_eq!(files[2].status, FileStatus::Renamed);
        assert_eq!(files[2].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(files[2].new_path.as_deref(), Some("new name.rs"), "git's trailing tab is not part of the name");
        assert!(files[3].binary);
        assert_eq!(files[3].path(), "logo.png");
        assert!(files[3].hunks.is_empty());
        assert_eq!(files[4].status, FileStatus::ModeChanged);
        assert_eq!(files[4].path(), "run.sh");
        assert_eq!((files[4].old_mode.as_deref(), files[4].new_mode.as_deref()), (Some("100644"), Some("100755")));
        assert_eq!(files[5].status, FileStatus::Renamed);
        assert_eq!(files[5].old_path.as_deref(), Some("moved.txt"));
        assert_eq!(files[5].path(), "elsewhere.txt");

        // Whole-file patches keep their new/deleted header; a rename is
        // written against the new path only.
        let p = hunk_patch(&files[0], &files[0].hunks[0]);
        assert!(p.contains("new file mode 100755\n--- /dev/null\n+++ b/new.txt\n"), "{p}");
        let p = hunk_patch(&files[1], &files[1].hunks[0]);
        assert!(p.contains("deleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n"), "{p}");
        let p = hunk_patch(&files[2], &files[2].hunks[0]);
        assert!(p.contains("--- a/new name.rs\t\n+++ b/new name.rs\t\n"), "{p}");
        assert!(!p.contains("rename"), "{p}");
    }

    #[test]
    fn quoting_round_trips() {
        assert_eq!(unquote("\"a/sp\\303\\251c\\\"ial\\ttab\""), "a/spéc\"ial\ttab");
        assert_eq!(unquote("plain"), "plain");
        for p in ["plain/path.rs", "with \"quote\"", "back\\slash", "tab\there", "nl\nx"] {
            let q = quote(p);
            assert_eq!(unquote(&q), p, "{q}");
        }
        assert_eq!(quote("no/quoting needed"), "no/quoting needed");
        let files = parse("diff --git \"a/x\\\"y\" \"b/x\\\"y\"\nindex 1..2 100644\nBinary files \"a/x\\\"y\" and \"b/x\\\"y\" differ\n");
        assert_eq!(files[0].path(), "x\"y");
    }

    #[test]
    fn crlf_is_kept_raw() {
        let files = parse("diff --git a/w.txt b/w.txt\n--- a/w.txt\n+++ b/w.txt\n@@ -1 +1 @@\n-a\r\n+b\r\n");
        assert_eq!(files[0].hunks[0].lines[0].text, "a\r");
        let p = hunk_patch(&files[0], &files[0].hunks[0]);
        assert!(p.contains("-a\r\n+b\r\n"));
    }

    #[test]
    fn content_that_looks_like_a_header_stays_content() {
        let patch = "diff --git a/m.diff b/m.diff\n--- a/m.diff\n+++ b/m.diff\n@@ -1,2 +1,2 @@\n--- a/x\n-+++ b/x\n+@@ -1 +1 @@\n+diff --git a/q b/q\n";
        let files = parse(patch);
        assert_eq!(files.len(), 1);
        let texts: Vec<_> = files[0].hunks[0].lines.iter().map(|l| (l.kind, l.text.as_str())).collect();
        assert_eq!(
            texts,
            vec![
                (LineKind::Del, "-- a/x"),
                (LineKind::Del, "+++ b/x"),
                (LineKind::Add, "@@ -1 +1 @@"),
                (LineKind::Add, "diff --git a/q b/q")
            ]
        );
        assert!(parse("").is_empty());
        assert!(parse("garbage\nmore").is_empty());
    }
}
