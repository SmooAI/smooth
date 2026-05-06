//! Render mutating tool calls as styled unified diffs.
//!
//! For `edit_file`, `write_file`, and `apply_patch`, the bare
//! "tool_name(args)" tool-call header buries the actually-interesting
//! information — the change. This module turns the captured tool
//! arguments into a colored unified diff (path header, hunks,
//! red-`-` / green-`+` / muted-context lines) the operator can read
//! at a glance.
//!
//! Only mutating-on-disk tools get diff rendering. Bash, read_file,
//! grep, and friends fall back to the existing
//! "header + collapsed output" rendering in `inline::message_lines`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use similar::TextDiff;

/// Maximum context lines around each change. Wider than `similar`'s
/// default `0` so the agent's edits are readable but tight enough that
/// big files don't drown the chat.
const CONTEXT_RADIUS: usize = 2;

/// Maximum lines we'll render per file. A 5000-line `write_file`
/// shouldn't paint 5000 lines of `+` into the chat. Beyond this we
/// elide the middle and show a `… (N more lines elided)` marker.
const MAX_DIFF_LINES: usize = 200;

/// Try to render a tool call as a unified diff. Returns `None` for
/// tools that aren't disk-mutating or when we can't parse the
/// arguments (the caller falls back to the standard collapsed-output
/// rendering).
#[must_use]
pub fn render(tool_name: &str, arguments: &Value) -> Option<Vec<Line<'static>>> {
    match tool_name {
        "edit_file" => render_edit_file(arguments),
        "write_file" => render_write_file(arguments),
        "apply_patch" => render_apply_patch(arguments),
        _ => None,
    }
}

fn render_edit_file(args: &Value) -> Option<Vec<Line<'static>>> {
    let path = args.get("path")?.as_str()?.to_string();
    let old_string = args.get("old_string")?.as_str().unwrap_or("");
    let new_string = args.get("new_string")?.as_str().unwrap_or("");
    Some(unified_diff_lines(&path, old_string, new_string))
}

fn render_write_file(args: &Value) -> Option<Vec<Line<'static>>> {
    let path = args.get("path")?.as_str()?.to_string();
    let content = args.get("content")?.as_str().unwrap_or("");
    // write_file is "create or overwrite": render every line as added.
    // Diffing against empty produces an all-`+` block, which is what
    // we want.
    Some(unified_diff_lines(&path, "", content))
}

fn render_apply_patch(args: &Value) -> Option<Vec<Line<'static>>> {
    // apply_patch already takes a unified-diff string. Render it
    // verbatim with the same styling we use for the other tools so the
    // visual is consistent across all three.
    let patch = args.get("patch").or_else(|| args.get("diff")).or_else(|| args.get("input"))?.as_str()?;
    Some(style_unified_diff(patch))
}

/// Build a unified diff between `old` and `new` for `path` and apply
/// our styling. Falls back to a single "no changes" line when the two
/// strings are identical.
fn unified_diff_lines(path: &str, old: &str, new: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(format!("  ▸ {path}"), header_style())));

    if old == new {
        lines.push(Line::from(Span::styled("    (no changes)".to_string(), muted())));
        return lines;
    }

    let diff = TextDiff::from_lines(old, new);
    let unified = diff.unified_diff().context_radius(CONTEXT_RADIUS).header("a", "b").to_string();
    lines.extend(style_unified_diff(&unified));
    lines
}

/// Walk a unified-diff string and turn each line into a styled
/// `Line<'static>`. Skips the synthetic `--- a` / `+++ b` headers
/// `similar` produces — we already drew our own path header.
fn style_unified_diff(diff: &str) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let total: Vec<&str> = diff.lines().collect();
    let trimmed = trim_diff_for_display(&total);

    for line in trimmed {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            // Skip — we render our own path header.
            continue;
        }
        let (prefix_style, content_style) = match line.chars().next() {
            Some('+') => (added_prefix(), added_content()),
            Some('-') => (removed_prefix(), removed_content()),
            Some('@') => (hunk_style(), hunk_style()),
            _ => (muted(), muted()),
        };
        // First char is the diff marker (+/-/space/@); style the
        // marker and the rest distinctly so the marker pops without
        // washing out the code itself.
        let mut chars = line.chars();
        let marker = chars.next().map(String::from).unwrap_or_default();
        let rest: String = chars.collect();
        out.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(marker, prefix_style),
            Span::styled(rest, content_style),
        ]));
    }
    out
}

/// Cap the rendered diff at `MAX_DIFF_LINES`. Keeps the head and tail
/// and replaces the middle with an elision marker so very large
/// changes don't drown the chat.
fn trim_diff_for_display<'a>(lines: &'a [&'a str]) -> Vec<String> {
    if lines.len() <= MAX_DIFF_LINES {
        return lines.iter().map(|s| (*s).to_string()).collect();
    }
    let keep_each_side = MAX_DIFF_LINES / 2 - 2;
    let head = lines.iter().take(keep_each_side).map(|s| (*s).to_string());
    let tail = lines.iter().skip(lines.len() - keep_each_side).map(|s| (*s).to_string());
    let elided = lines.len() - 2 * keep_each_side;
    let marker = format!("… {elided} more diff lines elided …");
    head.chain(std::iter::once(marker)).chain(tail).collect()
}

fn header_style() -> Style {
    Style::default().fg(Color::Rgb(0xff, 0x9f, 0x43)).add_modifier(Modifier::BOLD)
}

fn muted() -> Style {
    Style::default().fg(Color::Rgb(0x88, 0x88, 0x95))
}

fn added_prefix() -> Style {
    Style::default().fg(Color::Rgb(0x6f, 0xcf, 0x97)).add_modifier(Modifier::BOLD)
}

fn added_content() -> Style {
    Style::default().fg(Color::Rgb(0xb6, 0xe2, 0xc1))
}

fn removed_prefix() -> Style {
    Style::default().fg(Color::Rgb(0xff, 0x6b, 0x6b)).add_modifier(Modifier::BOLD)
}

fn removed_content() -> Style {
    Style::default().fg(Color::Rgb(0xff, 0xa6, 0xa6))
}

fn hunk_style() -> Style {
    Style::default().fg(Color::Rgb(0x82, 0xb1, 0xff))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn unknown_tool_returns_none() {
        assert!(render("bash", &json!({"command": "ls"})).is_none());
        assert!(render("read_file", &json!({"path": "x"})).is_none());
    }

    #[test]
    fn edit_file_emits_path_header_plus_diff() {
        let args = json!({
            "path": "src/lib.rs",
            "old_string": "let x = 1;\n",
            "new_string": "let x = 2;\n",
        });
        let lines = render("edit_file", &args).expect("edit_file should render");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("src/lib.rs"));
        assert!(combined.contains("-let x = 1"));
        assert!(combined.contains("+let x = 2"));
    }

    #[test]
    fn edit_file_no_change_collapses() {
        let args = json!({
            "path": "src/lib.rs",
            "old_string": "same\n",
            "new_string": "same\n",
        });
        let lines = render("edit_file", &args).expect("edit_file should render");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("(no changes)"));
    }

    #[test]
    fn write_file_renders_all_added() {
        let args = json!({
            "path": "src/new.rs",
            "content": "fn hello() {}\n",
        });
        let lines = render("write_file", &args).expect("write_file should render");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("src/new.rs"));
        assert!(combined.contains("+fn hello"));
    }

    #[test]
    fn apply_patch_uses_provided_diff() {
        let patch = "--- a/foo\n+++ b/foo\n@@ -1 +1 @@\n-old\n+new\n";
        let args = json!({"patch": patch});
        let lines = render("apply_patch", &args).expect("apply_patch should render");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("@@ -1 +1 @@"));
        assert!(combined.contains("-old"));
        assert!(combined.contains("+new"));
    }

    #[test]
    fn missing_required_field_falls_back_to_none() {
        // edit_file without `path` shouldn't panic — return None and let
        // the caller fall back to the standard tool-call rendering.
        let args = json!({"old_string": "a", "new_string": "b"});
        assert!(render("edit_file", &args).is_none());
    }

    #[test]
    fn large_diff_is_elided() {
        let big_old = (0..400).map(|i| format!("line {i}\n")).collect::<String>();
        let big_new = (0..400).map(|i| format!("LINE {i}\n")).collect::<String>();
        let args = json!({"path": "huge.txt", "old_string": big_old, "new_string": big_new});
        let lines = render("edit_file", &args).expect("should render");
        let combined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("more diff lines elided"));
        // Cap is MAX_DIFF_LINES (200) + the path header + maybe a few
        // synthetic markers; assert we're well under what the
        // un-elided version would be (~1600 lines for radius=2).
        assert!(lines.len() < 250, "expected elision to keep under 250 lines, got {}", lines.len());
    }
}
