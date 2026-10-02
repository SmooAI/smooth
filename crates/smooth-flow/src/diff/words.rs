//! Word-level change spans inside a changed line (the delta / Kaleidoscope
//! highlight): pair the deleted and added lines of a change block, diff each
//! pair token by token, and mark what differs.

use similar::{Algorithm, DiffOp};

use super::model::LineKind;

/// Lines longer than this (chars) get no word spans — minified output.
pub const MAX_WORD_LINE_CHARS: usize = 1000;
/// When more than this share of a line changed, the pair is a rewrite, and
/// highlighting nearly all of it says less than highlighting none.
pub const REWRITE_RATIO: f64 = 0.6;

/// Split into tokens: runs of word characters, runs of whitespace, and
/// single other characters. Returns `(start, end)` in char offsets.
#[must_use]
pub fn tokens(line: &str) -> Vec<(usize, usize, &str)> {
    #[derive(PartialEq, Eq, Clone, Copy)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut out = Vec::new();
    let mut start_char = 0usize;
    let mut start_byte = 0usize;
    let mut prev: Option<Class> = None;
    for (ci, (bi, c)) in line.char_indices().enumerate() {
        let k = class(c);
        let joins = prev == Some(k) && k != Class::Other;
        if !joins && ci > 0 {
            out.push((start_char, ci, &line[start_byte..bi]));
            start_char = ci;
            start_byte = bi;
        }
        prev = Some(k);
    }
    let n = line.chars().count();
    if n > 0 {
        out.push((start_char, n, &line[start_byte..]));
    }
    out
}

fn merge(spans: &mut Vec<[u32; 2]>) {
    spans.sort_unstable();
    let mut merged: Vec<[u32; 2]> = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        if let Some(last) = merged.last_mut() {
            if s[0] <= last[1] {
                last[1] = last[1].max(s[1]);
                continue;
            }
        }
        merged.push(s);
    }
    *spans = merged;
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The changed spans of `old` and of `new`, or `None` when the pair is not
/// worth highlighting (too long, or a rewrite).
#[must_use]
pub fn pair_spans(old: &str, new: &str) -> Option<(Vec<[u32; 2]>, Vec<[u32; 2]>)> {
    let (old_len, new_len) = (old.chars().count(), new.chars().count());
    if old_len > MAX_WORD_LINE_CHARS || new_len > MAX_WORD_LINE_CHARS {
        return None;
    }
    let a = tokens(old);
    let b = tokens(new);
    let at: Vec<&str> = a.iter().map(|t| t.2).collect();
    let bt: Vec<&str> = b.iter().map(|t| t.2).collect();
    let ops = similar::capture_diff_slices(Algorithm::Myers, &at, &bt);
    let mut os = Vec::new();
    let mut ns = Vec::new();
    for op in ops {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => {
                os.push([to_u32(a[old_index].0), to_u32(a[old_index + old_len - 1].1)]);
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                ns.push([to_u32(b[new_index].0), to_u32(b[new_index + new_len - 1].1)]);
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                os.push([to_u32(a[old_index].0), to_u32(a[old_index + old_len - 1].1)]);
                ns.push([to_u32(b[new_index].0), to_u32(b[new_index + new_len - 1].1)]);
            }
        }
    }
    merge(&mut os);
    merge(&mut ns);
    let covered = |spans: &[[u32; 2]]| spans.iter().map(|s| f64::from(s[1] - s[0])).sum::<f64>();
    let total = f64::from(to_u32(old_len.max(new_len)).max(1));
    if covered(&os).max(covered(&ns)) / total > REWRITE_RATIO {
        return None;
    }
    Some((os, ns))
}

/// Word spans for every line of a hunk, by line index. A change block is a
/// run of `del` lines followed by a run of `add` lines; its i-th del pairs
/// with its i-th add, and unpaired lines get no spans.
#[must_use]
pub fn hunk_word_spans(lines: &[(LineKind, &str)]) -> Vec<Vec<[u32; 2]>> {
    let mut out = vec![Vec::new(); lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].0 != LineKind::Del {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < lines.len() && lines[i].0 == LineKind::Del {
            i += 1;
        }
        let add_start = i;
        while i < lines.len() && lines[i].0 == LineKind::Add {
            i += 1;
        }
        let dels = add_start - del_start;
        let adds = i - add_start;
        for k in 0..dels.min(adds) {
            let (d, a) = (del_start + k, add_start + k);
            if let Some((os, ns)) = pair_spans(lines[d].1, lines[a].1) {
                out[d] = os;
                out[a] = ns;
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_words_space_and_punctuation() {
        let t: Vec<_> = tokens("let x_1 = foo(a,b);").into_iter().map(|t| t.2).collect();
        assert_eq!(t, vec!["let", " ", "x_1", " ", "=", " ", "foo", "(", "a", ",", "b", ")", ";"]);
        assert!(tokens("").is_empty());
        let t = tokens("é ü");
        assert_eq!(t[2].0, 2, "char offsets, not bytes");
    }

    #[test]
    fn marks_only_the_changed_word() {
        let (o, n) = pair_spans("let count = 1;", "let total = 1;").unwrap();
        assert_eq!(o, vec![[4, 9]]);
        assert_eq!(n, vec![[4, 9]]);
        let (o, n) = pair_spans("foo(a)", "foo(a, b)").unwrap();
        assert!(o.is_empty());
        assert_eq!(n, vec![[5, 8]]);
    }

    #[test]
    fn unicode_offsets_are_chars() {
        let (o, n) = pair_spans("café = 1", "café = 2").unwrap();
        assert_eq!(o, vec![[7, 8]]);
        assert_eq!(n, vec![[7, 8]]);
    }

    #[test]
    fn rewrites_and_huge_lines_get_nothing() {
        assert!(pair_spans("completely different", "nothing alike here at all").is_none());
        let long = "x".repeat(MAX_WORD_LINE_CHARS + 1);
        assert!(pair_spans(&long, "x").is_none());
    }

    #[test]
    fn pairs_within_change_blocks_only() {
        let lines = [
            (LineKind::Ctx, "a"),
            (LineKind::Del, "let a = 1;"),
            (LineKind::Del, "let b = 2;"),
            (LineKind::Add, "let a = 10;"),
            (LineKind::Ctx, "z"),
            (LineKind::Add, "let b = 2;"),
        ];
        let w = hunk_word_spans(&lines);
        assert_eq!(w[1], vec![[8, 9]]);
        assert_eq!(w[3], vec![[8, 10]]);
        assert!(w[2].is_empty(), "unpaired del");
        assert!(w[5].is_empty(), "an add after context starts a new block with no dels");
        assert!(w[0].is_empty() && w[4].is_empty());
    }
}
