//! Markdown renderer — pulldown-cmark tokens to ratatui spans.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::theme;

/// Render markdown string into ratatui Text.
pub fn render(markdown: &str) -> Text<'static> {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let parser = Parser::new_ext(markdown, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut in_code_block = false;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    flush_line(&mut lines, &mut current_spans);
                    let style = match level {
                        pulldown_cmark::HeadingLevel::H1 => theme::title(),
                        pulldown_cmark::HeadingLevel::H2 => theme::subtitle(),
                        _ => Style::default().add_modifier(Modifier::BOLD),
                    };
                    style_stack.push(style);
                }
                Tag::Paragraph => {}
                Tag::Strong => {
                    style_stack.push(current_style(&style_stack).add_modifier(Modifier::BOLD));
                }
                Tag::Emphasis => {
                    style_stack.push(current_style(&style_stack).add_modifier(Modifier::ITALIC));
                }
                Tag::Strikethrough => {
                    style_stack.push(current_style(&style_stack).add_modifier(Modifier::CROSSED_OUT));
                }
                Tag::CodeBlock(_) => {
                    flush_line(&mut lines, &mut current_spans);
                    in_code_block = true;
                    style_stack.push(Style::default().fg(Color::Gray));
                    // Border line
                    lines.push(Line::from(Span::styled(
                        "╭─────────────────────────────────────",
                        Style::default().fg(theme::SMOO_GREEN),
                    )));
                }
                Tag::List(_) => {}
                Tag::Item => {
                    flush_line(&mut lines, &mut current_spans);
                    current_spans.push(Span::styled("  ● ", theme::title()));
                }
                Tag::Link { dest_url: _, .. } => {
                    style_stack.push(Style::default().fg(theme::SMOO_GREEN).add_modifier(Modifier::UNDERLINED));
                }
                Tag::BlockQuote(_) => {
                    style_stack.push(Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC));
                    current_spans.push(Span::styled("│ ", Style::default().fg(theme::SMOO_GREEN)));
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    flush_line(&mut lines, &mut current_spans);
                    style_stack.pop();
                    lines.push(Line::default()); // Spacing after heading
                }
                TagEnd::Paragraph => {
                    flush_line(&mut lines, &mut current_spans);
                    lines.push(Line::default()); // Spacing after paragraph
                }
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link | TagEnd::BlockQuote(_) => {
                    style_stack.pop();
                }
                TagEnd::CodeBlock => {
                    flush_line(&mut lines, &mut current_spans);
                    lines.push(Line::from(Span::styled(
                        "╰─────────────────────────────────────",
                        Style::default().fg(theme::SMOO_GREEN),
                    )));
                    in_code_block = false;
                    style_stack.pop();
                }
                TagEnd::Item => {
                    flush_line(&mut lines, &mut current_spans);
                }
                _ => {}
            },
            Event::Text(text) => {
                let style = current_style(&style_stack);
                if in_code_block {
                    // Code block: render each line separately
                    for (i, line) in text.lines().enumerate() {
                        if i > 0 {
                            flush_line(&mut lines, &mut current_spans);
                            current_spans.push(Span::styled("│ ", Style::default().fg(theme::SMOO_GREEN)));
                        } else {
                            current_spans.push(Span::styled("│ ", Style::default().fg(theme::SMOO_GREEN)));
                        }
                        current_spans.push(Span::styled(line.to_string(), style));
                    }
                } else {
                    current_spans.push(Span::styled(text.to_string(), style));
                }
            }
            Event::Code(code) => {
                current_spans.push(Span::styled(format!(" {code} "), Style::default().fg(theme::SMOO_GREEN)));
            }
            Event::SoftBreak | Event::HardBreak => {
                flush_line(&mut lines, &mut current_spans);
            }
            Event::Rule => {
                flush_line(&mut lines, &mut current_spans);
                lines.push(Line::from(Span::styled("─".repeat(40), theme::muted())));
            }
            _ => {}
        }
    }

    flush_line(&mut lines, &mut current_spans);
    Text::from(lines)
}

fn current_style(stack: &[Style]) -> Style {
    stack.last().copied().unwrap_or_default()
}

fn flush_line(lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>) {
    if !spans.is_empty() {
        lines.push(Line::from(std::mem::take(spans)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_heading() {
        let text = render("# Hello");
        assert!(!text.lines.is_empty());
    }

    #[test]
    fn test_render_paragraph() {
        let text = render("Hello world");
        assert!(!text.lines.is_empty());
    }

    #[test]
    fn test_render_code_block() {
        let text = render("```\nlet x = 1;\n```");
        let content = format!("{text:?}");
        assert!(content.contains("╭"));
    }

    #[test]
    fn test_render_list() {
        let text = render("- item 1\n- item 2");
        let content = format!("{text:?}");
        assert!(content.contains("●"));
    }

    #[test]
    fn test_render_empty() {
        let text = render("");
        assert!(text.lines.is_empty());
    }

    #[test]
    fn test_render_inline_code() {
        let text = render("Use `th up` to start");
        let content = format!("{text:?}");
        assert!(content.contains("th up"));
    }
}
