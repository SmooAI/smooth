//! Server-side syntax spans (th-26f5b9).
//!
//! syntect's parser over bat's syntax set (`two-face`), scopes folded onto
//! the small [`TokenKind`] vocabulary every client maps to its theme. No theme ships with the engine — a theme
//! is a client's business.
//!
//! **Why syntect + two-face, not tree-sitter.** Pure Rust (the `regex-fancy`
//! engine, no Oniguruma C build), one dependency that covers ~200 languages
//! from a single compressed dump, versus one C grammar crate per language
//! for tree-sitter-highlight. Measured on a stripped release binary the pair
//! costs ~1.3 MB (199 syntaxes incl. TypeScript/TSX, Swift, Kotlin, TOML,
//! which syntect's own default set lacks). Parsing is line-oriented and
//! stateful, which suits hunks: each hunk side is highlighted as its own
//! stream (the way delta does it), so a hunk that starts inside a block
//! comment can be off until the comment closes — a known, accepted limit.

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use super::model::TokenKind;

/// Lines longer than this (bytes) stop highlighting for the rest of the
/// stream: the regex engine's worst cases live on minified one-liners.
pub const MAX_SYNTAX_LINE_BYTES: usize = 2000;

fn set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// Scope prefix → kind, most specific first. The first scope on the stack
/// (innermost outward) with any match decides.
fn table() -> &'static [(Scope, TokenKind)] {
    static TABLE: OnceLock<Vec<(Scope, TokenKind)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        [
            ("comment", TokenKind::Comment),
            ("punctuation.definition.comment", TokenKind::Comment),
            ("constant.character.escape", TokenKind::Escape),
            ("string", TokenKind::String),
            ("punctuation.definition.string", TokenKind::String),
            ("constant.numeric", TokenKind::Number),
            ("constant.language", TokenKind::Constant),
            ("constant", TokenKind::Constant),
            ("support.constant", TokenKind::Constant),
            ("variable.language", TokenKind::Constant),
            ("entity.name.function.macro", TokenKind::Macro),
            ("entity.name.macro", TokenKind::Macro),
            ("support.function.macro", TokenKind::Macro),
            ("support.macro", TokenKind::Macro),
            ("entity.name.function", TokenKind::Function),
            ("support.function", TokenKind::Function),
            ("variable.function", TokenKind::Function),
            ("meta.function-call.identifier", TokenKind::Function),
            ("entity.name.type", TokenKind::Type),
            ("entity.name.class", TokenKind::Type),
            ("entity.name.struct", TokenKind::Type),
            ("entity.name.enum", TokenKind::Type),
            ("entity.name.trait", TokenKind::Type),
            ("entity.name.interface", TokenKind::Type),
            ("entity.name.impl", TokenKind::Type),
            ("entity.other.inherited-class", TokenKind::Type),
            ("support.type", TokenKind::Type),
            ("support.class", TokenKind::Type),
            ("keyword.operator", TokenKind::Operator),
            ("keyword", TokenKind::Keyword),
            ("storage", TokenKind::Keyword),
            ("entity.name.tag", TokenKind::Tag),
            ("entity.other.attribute-name", TokenKind::Attribute),
            ("meta.attribute", TokenKind::Attribute),
            ("variable.other.member", TokenKind::Property),
            ("variable.other.property", TokenKind::Property),
            ("support.variable.property", TokenKind::Property),
            ("meta.mapping.key", TokenKind::Property),
            ("variable", TokenKind::Variable),
            ("markup.heading", TokenKind::Heading),
            ("markup.underline.link", TokenKind::Link),
            ("markup.link", TokenKind::Link),
            ("markup.raw", TokenKind::String),
            ("punctuation", TokenKind::Punctuation),
        ]
        .into_iter()
        .filter_map(|(s, k)| Scope::new(s).ok().map(|s| (s, k)))
        .collect()
    })
}

fn kind_of(stack: &ScopeStack) -> Option<TokenKind> {
    let table = table();
    for scope in stack.as_slice().iter().rev() {
        if let Some((_, k)) = table.iter().find(|(prefix, _)| prefix.is_prefix_of(*scope)) {
            return Some(*k);
        }
    }
    None
}

/// The syntax for a path: by full file name first (`Makefile`,
/// `Dockerfile`), then by extension. `None` for plain text.
#[must_use]
pub fn syntax_for(path: &str) -> Option<&'static SyntaxReference> {
    let ss = set();
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e);
    ss.find_syntax_by_extension(name)
        .or_else(|| ext.and_then(|e| ss.find_syntax_by_extension(e)))
        .or_else(|| ext.and_then(|e| ss.find_syntax_by_extension(&e.to_ascii_lowercase())))
        .filter(|s| s.name != "Plain Text")
}

/// One highlighting stream (one side of one hunk).
pub struct Highlighter {
    parse: ParseState,
    stack: ScopeStack,
    alive: bool,
}

impl Highlighter {
    #[must_use]
    pub fn new(syntax: &SyntaxReference) -> Self {
        Self {
            parse: ParseState::new(syntax),
            stack: ScopeStack::new(),
            alive: true,
        }
    }

    /// Spans `[start, end, kind]` (char offsets) for the next line of the
    /// stream. Empty once the stream gave up (a too-long line, a parse error).
    pub fn line(&mut self, text: &str) -> Vec<[u32; 3]> {
        if !self.alive {
            return Vec::new();
        }
        if text.len() > MAX_SYNTAX_LINE_BYTES {
            self.alive = false;
            return Vec::new();
        }
        let with_nl = format!("{text}\n");
        let Ok(ops) = self.parse.parse_line(&with_nl, set()) else {
            self.alive = false;
            return Vec::new();
        };
        let mut spans: Vec<[u32; 3]> = Vec::new();
        let mut last_byte = 0usize;
        let mut last_char = 0usize;
        let mut push = |stack: &ScopeStack, from_char: usize, to_char: usize| {
            if to_char <= from_char {
                return;
            }
            let Some(k) = kind_of(stack) else { return };
            let (s, e, k) = (
                u32::try_from(from_char).unwrap_or(u32::MAX),
                u32::try_from(to_char).unwrap_or(u32::MAX),
                u32::from(k as u8),
            );
            if let Some(prev) = spans.last_mut() {
                if prev[1] == s && prev[2] == k {
                    prev[1] = e;
                    return;
                }
            }
            spans.push([s, e, k]);
        };
        for (pos, op) in ops {
            let pos = pos.min(text.len());
            if pos > last_byte && text.is_char_boundary(pos) {
                let to_char = last_char + text[last_byte..pos].chars().count();
                push(&self.stack, last_char, to_char);
                last_byte = pos;
                last_char = to_char;
            }
            if self.stack.apply(&op).is_err() {
                self.alive = false;
                return Vec::new();
            }
        }
        if last_byte < text.len() {
            let to_char = last_char + text[last_byte..].chars().count();
            push(&self.stack, last_char, to_char);
        }
        spans
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn kinds(path: &str, line: &str) -> Vec<(String, &'static str)> {
        let mut h = Highlighter::new(syntax_for(path).unwrap());
        let chars: Vec<char> = line.chars().collect();
        h.line(line)
            .into_iter()
            .map(|[s, e, k]| (chars[s as usize..e as usize].iter().collect(), TokenKind::NAMES[k as usize]))
            .collect()
    }

    #[test]
    fn finds_languages_by_name_and_extension() {
        assert_eq!(syntax_for("src/lib.rs").unwrap().name, "Rust");
        assert_eq!(syntax_for("a/b/App.tsx").unwrap().name, "TypeScriptReact");
        assert_eq!(syntax_for("x.ts").unwrap().name, "TypeScript");
        assert_eq!(syntax_for("Sources/UI/View.swift").unwrap().name, "Swift");
        assert_eq!(syntax_for("Cargo.toml").unwrap().name, "TOML");
        assert_eq!(syntax_for("Makefile").unwrap().name, "Makefile");
        assert!(syntax_for("notes.unknownext").is_none());
        assert!(syntax_for("README").is_none());
    }

    #[test]
    fn rust_tokens_map_to_kinds() {
        let k = kinds("a.rs", "pub fn add(x: u32) -> u32 { x + 1 } // sum");
        assert!(k.contains(&("pub".into(), "keyword")), "{k:?}");
        assert!(k.contains(&("add".into(), "function")), "{k:?}");
        let k2 = kinds("a.rs", "struct Point { x: i32 }");
        assert!(k2.contains(&("Point".into(), "type")), "{k2:?}");
        assert!(k.contains(&("1".into(), "number")), "{k:?}");
        assert!(k.iter().any(|(t, kind)| t.contains("sum") && *kind == "comment"), "{k:?}");
    }

    #[test]
    fn strings_include_their_quotes_and_offsets_are_chars() {
        let k = kinds("a.py", "s = \"héllo\"");
        assert!(k.contains(&("\"héllo\"".into(), "string")), "{k:?}");
    }

    #[test]
    fn state_carries_across_lines_and_long_lines_stop_the_stream() {
        let mut h = Highlighter::new(syntax_for("a.rs").unwrap());
        h.line("/* open");
        let spans = h.line("still comment */ fn x() {}");
        assert_eq!(spans[0][2], TokenKind::Comment as u32, "{spans:?}");
        let long = "x".repeat(MAX_SYNTAX_LINE_BYTES + 1);
        assert!(h.line(&long).is_empty());
        assert!(h.line("fn y() {}").is_empty(), "the stream gave up");
    }
}
