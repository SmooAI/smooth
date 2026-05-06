//! Render markdown text into ratatui [`Line`]s with style.
//!
//! LLM responses arrive as markdown — bold, italic, inline code, fenced
//! code blocks, headings, lists. The chat panel used to print the raw
//! source so users saw literal `**`, backticks, and ``` lines. This
//! module walks the [`pulldown_cmark`] event stream and emits styled
//! [`Line`]s the chat renderer can paste straight into a [`Paragraph`].
//!
//! Streaming-friendly: when the buffer ends mid-fence the parser still
//! yields the events it has, so a partial code block renders as an
//! in-progress code block rather than a chunk of raw text.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Convert a markdown source string into styled [`Line`]s.
///
/// Returns one or more lines; an empty input yields a single empty line
/// so callers don't have to special-case it.
pub fn render(source: &str) -> Vec<Line<'static>> {
    if source.is_empty() {
        return vec![Line::from("")];
    }

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(source, opts);

    let mut state = RenderState::default();
    for event in parser {
        state.handle(event);
    }
    state.finish();
    if state.lines.is_empty() {
        vec![Line::from("")]
    } else {
        state.lines
    }
}

#[derive(Default)]
struct RenderState {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    /// Active style modifiers (bold/italic/strikethrough) — pushed on
    /// each opening Tag, popped on the matching End. Tracked as a stack
    /// so nested formatting (e.g. **_bold-italic_**) composes.
    style_stack: Vec<Style>,
    /// Inside a fenced or indented code block — text events get the
    /// code style and each newline closes the current line.
    in_code_block: bool,
    /// Current list nesting depth (0 = not in a list). Used to pick
    /// the bullet glyph and the indent.
    list_depth: usize,
    /// `Some(n)` = ordered list with the next item number; `None` =
    /// unordered.
    list_kind: Vec<Option<u64>>,
    /// Inside a blockquote — prefixes new lines with `│ `.
    in_blockquote: bool,
    /// Inside a heading — applied as a single bold + colored style on
    /// finish.
    heading: Option<HeadingLevel>,
}

impl RenderState {
    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(t.into_string()),
            Event::Code(c) => {
                self.push_span(c.into_string(), code_style());
            }
            Event::SoftBreak => {
                // Treat soft breaks as a single space so wrapped
                // markdown source doesn't fragment the output.
                self.push_span(" ".to_string(), self.merged_style());
            }
            Event::HardBreak => {
                self.push_line();
            }
            Event::Rule => {
                self.push_line();
                self.lines.push(Line::from(Span::styled("─".repeat(40), muted())));
            }
            // Tables, footnotes, html, tasklist markers, math: not handled.
            // The text content (if any) still flows through subsequent
            // Text events, so we don't lose information — just style.
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.push_line();
                self.heading = Some(level);
            }
            Tag::CodeBlock(_) => {
                self.push_line();
                self.in_code_block = true;
            }
            Tag::List(start) => {
                if self.list_depth == 0 {
                    self.push_line();
                }
                self.list_depth += 1;
                self.list_kind.push(start);
            }
            Tag::Item => {
                self.push_line();
                let depth = self.list_depth.saturating_sub(1);
                let indent = "  ".repeat(depth);
                let bullet = match self.list_kind.last_mut() {
                    Some(Some(n)) => {
                        let s = format!("{n}. ");
                        *n += 1;
                        s
                    }
                    _ => "• ".to_string(),
                };
                self.current.push(Span::raw(format!("{indent}{bullet}")));
            }
            Tag::Emphasis => self.push_modifier(Modifier::ITALIC),
            Tag::Strong => self.push_modifier(Modifier::BOLD),
            Tag::Strikethrough => self.push_modifier(Modifier::CROSSED_OUT),
            Tag::BlockQuote(_) => {
                self.push_line();
                self.in_blockquote = true;
            }
            Tag::Link { .. } => {
                self.push_modifier(Modifier::UNDERLINED);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.push_line();
                self.lines.push(Line::from(""));
            }
            TagEnd::Heading(level) => {
                let style = heading_style(level);
                for span in &mut self.current {
                    span.style = span.style.patch(style);
                }
                self.push_line();
                self.heading = None;
                self.lines.push(Line::from(""));
            }
            TagEnd::CodeBlock => {
                self.push_line();
                self.in_code_block = false;
                self.lines.push(Line::from(""));
            }
            TagEnd::List(_) => {
                self.push_line();
                self.list_depth = self.list_depth.saturating_sub(1);
                self.list_kind.pop();
                if self.list_depth == 0 {
                    self.lines.push(Line::from(""));
                }
            }
            TagEnd::Item => self.push_line(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.style_stack.pop();
            }
            TagEnd::BlockQuote(_) => {
                self.push_line();
                self.in_blockquote = false;
                self.lines.push(Line::from(""));
            }
            _ => {}
        }
    }

    fn text(&mut self, t: String) {
        if self.in_code_block {
            // Code blocks preserve newlines verbatim — emit one styled
            // span per line so each ends up as its own Line.
            for (i, chunk) in t.split('\n').enumerate() {
                if i > 0 {
                    self.push_line();
                }
                if !chunk.is_empty() {
                    self.current.push(Span::styled(chunk.to_string(), code_style()));
                }
            }
        } else {
            self.push_span(t, self.merged_style());
        }
    }

    fn push_span(&mut self, content: String, style: Style) {
        if content.is_empty() {
            return;
        }
        self.current.push(Span::styled(content, style));
    }

    fn push_modifier(&mut self, modifier: Modifier) {
        let next = self.style_stack.last().copied().unwrap_or_default().add_modifier(modifier);
        self.style_stack.push(next);
    }

    fn merged_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }

    fn push_line(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.current);
        let line = if self.in_blockquote {
            let mut prefixed = vec![Span::styled("│ ", muted())];
            prefixed.extend(spans);
            Line::from(prefixed)
        } else {
            Line::from(spans)
        };
        self.lines.push(line);
    }

    fn finish(&mut self) {
        self.push_line();
        // Trim a single trailing blank line so consecutive messages
        // don't compound vertical whitespace.
        if let Some(last) = self.lines.last() {
            if last.spans.is_empty() {
                self.lines.pop();
            }
        }
    }
}

fn code_style() -> Style {
    Style::default().fg(Color::Rgb(0xc8, 0xa6, 0x6b)).bg(Color::Rgb(0x1c, 0x1c, 0x22))
}

fn muted() -> Style {
    Style::default().fg(Color::Rgb(0x88, 0x88, 0x95))
}

fn heading_style(level: HeadingLevel) -> Style {
    let base = Style::default().add_modifier(Modifier::BOLD);
    match level {
        HeadingLevel::H1 => base.fg(Color::Rgb(0xff, 0x9f, 0x43)),
        HeadingLevel::H2 => base.fg(Color::Rgb(0xff, 0xa6, 0x55)),
        HeadingLevel::H3 => base.fg(Color::Rgb(0xff, 0xb1, 0x6c)),
        _ => base.fg(Color::Rgb(0xff, 0xc4, 0x8d)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn empty_input_returns_one_line() {
        let lines = render("");
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "");
    }

    #[test]
    fn plain_paragraph_strips_no_text() {
        let lines = render("hello world");
        assert!(lines.iter().any(|l| line_text(l) == "hello world"));
    }

    #[test]
    fn bold_text_is_emitted_without_asterisks() {
        let lines = render("**bold** text");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("bold"));
        assert!(!combined.contains("**"));
    }

    #[test]
    fn inline_code_strips_backticks() {
        let lines = render("run `pnpm dev` to start");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("pnpm dev"));
        assert!(!combined.contains('`'));
    }

    #[test]
    fn fenced_code_block_preserves_lines() {
        let src = "```\nfoo\nbar\n```";
        let lines = render(src);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t == "foo"));
        assert!(texts.iter().any(|t| t == "bar"));
    }

    #[test]
    fn unterminated_fence_still_renders() {
        // Streaming case: buffer ends mid-fence.
        let lines = render("```\nfoo\nbar");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t == "foo"));
        assert!(texts.iter().any(|t| t == "bar"));
    }

    #[test]
    fn heading_strips_hashes() {
        let lines = render("# Title\n\nbody");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("Title"));
        assert!(!combined.contains("# Title"));
    }

    #[test]
    fn unordered_list_renders_bullets() {
        let lines = render("- one\n- two");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t.contains("• one")));
        assert!(texts.iter().any(|t| t.contains("• two")));
    }

    #[test]
    fn ordered_list_numbers_items() {
        let lines = render("1. first\n2. second");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t.contains("1. first")));
        assert!(texts.iter().any(|t| t.contains("2. second")));
    }

    #[test]
    fn blockquote_gets_prefix() {
        let lines = render("> quoted");
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t.contains("│ quoted")));
    }
}
