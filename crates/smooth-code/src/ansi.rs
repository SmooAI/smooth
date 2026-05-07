//! Parse ANSI SGR escape sequences into ratatui [`Span`]s.
//!
//! The runner emits structured tracing logs with ANSI SGR codes
//! (`\x1b[2m...\x1b[0m`, `\x1b[32m INFO`, `\x1b[3mfield\x1b[0m=value`).
//! Big Smooth forwards runner stderr as `TokenDelta` chunks for the
//! assistant message; if we leave the codes as plain text the
//! markdown renderer treats them as raw `[2m...[0m` litter, and if
//! we strip them we lose the readability the runner was conveying.
//! Parse them into Spans + Styles instead so the diagnostics render
//! with the colors the runner picked.
//!
//! Two input shapes are recognized:
//!
//! - **With ESC byte**: `\x1b[<params>m` — proper ANSI SGR.
//! - **Bare bracket form**: `[<digits>(;<digits>)*m` — sometimes the
//!   ESC byte is lost in transit (WebSocket marshalling that drops
//!   control bytes, terminal copy-paste). Same shape, no ESC.
//!
//! Conservative match: only sequences ending in `m` whose params are
//! digits + semicolons. Real chat content like markdown `[link]` and
//! array syntax `[1, 2, 3]` is preserved.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// `true` when the line contains an ANSI SGR sequence in either the
/// proper or bare-bracket form. Cheap pre-check the renderer uses to
/// decide between [`parse_line_to_spans`] and the markdown path.
#[must_use]
pub fn line_has_ansi(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if (b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[') || b == b'[' {
            let start = if b == 0x1b { i + 2 } else { i + 1 };
            if scan_sgr_params(bytes, start).is_some() {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Parse a single line of text containing ANSI SGR sequences into a
/// vector of styled ratatui `Span`s. Lines without any ANSI codes
/// produce a single plain Span; lines with codes get one Span per
/// style transition. The state machine resets on `0` (or empty
/// param) and accumulates modifiers / colors otherwise.
#[must_use]
pub fn parse_line_to_spans(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut current = Style::default();
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Detect ANSI SGR (with or without ESC).
        let (params_start, prefix_len) = if b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            (i + 2, 2_usize)
        } else if b == b'[' {
            (i + 1, 1_usize)
        } else {
            (0, 0_usize)
        };

        if prefix_len > 0 {
            if let Some(end_m) = scan_sgr_params(bytes, params_start) {
                // Valid SGR sequence: bytes[params_start..end_m] are
                // the params, bytes[end_m] is the closing `m`.
                let params_slice = &bytes[params_start..end_m];
                let params_str = std::str::from_utf8(params_slice).unwrap_or("");
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), current));
                }
                current = apply_sgr_params(current, params_str);
                i = end_m + 1;
                continue;
            }
        }

        // Not an ANSI sequence. Append the next UTF-8 char to buf.
        let ch_len = utf8_char_len(b);
        if ch_len == 0 {
            i += 1;
            continue;
        }
        let end = (i + ch_len).min(bytes.len());
        if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
            buf.push_str(s);
        }
        i = end;
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, current));
    }
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

/// Walk `digit (; digit)*` then expect a final `m`. Returns the byte
/// index of the closing `m` on success; `None` if the pattern doesn't
/// match.
fn scan_sgr_params(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    let mut saw_digit = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_digit() {
            saw_digit = true;
            i += 1;
            continue;
        }
        if b == b';' && saw_digit {
            i += 1;
            continue;
        }
        if b == b'm' && saw_digit {
            return Some(i);
        }
        return None;
    }
    None
}

/// Apply a `;`-separated SGR parameter string to a baseline style.
/// Supports: 0 reset, 1 bold, 2 dim, 3 italic, 4 underline,
/// 9 strikethrough, 22 / 23 / 24 / 29 (clear individual modifiers),
/// 30-37 fg, 39 default fg, 40-47 bg, 49 default bg,
/// 90-97 / 100-107 bright variants, 38;5;N + 48;5;N (256-color),
/// 38;2;R;G;B + 48;2;R;G;B (true color).
fn apply_sgr_params(mut style: Style, params: &str) -> Style {
    if params.is_empty() {
        return Style::default();
    }
    let nums: Vec<u16> = params.split(';').map(|p| p.parse::<u16>().unwrap_or(0)).collect();
    let mut i = 0;
    while i < nums.len() {
        let n = nums[i];
        match n {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD).remove_modifier(Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style = style.fg(basic_color((n - 30) as u8, false)),
            39 => style = style.fg(Color::Reset),
            40..=47 => style = style.bg(basic_color((n - 40) as u8, false)),
            49 => style = style.bg(Color::Reset),
            90..=97 => style = style.fg(basic_color((n - 90) as u8, true)),
            100..=107 => style = style.bg(basic_color((n - 100) as u8, true)),
            38 | 48 => {
                let is_fg = n == 38;
                if i + 1 < nums.len() {
                    match nums[i + 1] {
                        5 if i + 2 < nums.len() => {
                            let color = ansi_256_to_color(nums[i + 2].min(255) as u8);
                            style = if is_fg { style.fg(color) } else { style.bg(color) };
                            i += 2;
                        }
                        2 if i + 4 < nums.len() => {
                            let color = Color::Rgb(nums[i + 2].min(255) as u8, nums[i + 3].min(255) as u8, nums[i + 4].min(255) as u8);
                            style = if is_fg { style.fg(color) } else { style.bg(color) };
                            i += 4;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    style
}

/// Standard 16-color palette mapping (3/4-bit ANSI). `bright = true`
/// returns the 90-series (bright) variants. Uses ratatui's named
/// colors so the user's terminal palette controls the actual hue.
fn basic_color(idx: u8, bright: bool) -> Color {
    if bright {
        match idx {
            0 => Color::DarkGray,
            1 => Color::LightRed,
            2 => Color::LightGreen,
            3 => Color::LightYellow,
            4 => Color::LightBlue,
            5 => Color::LightMagenta,
            6 => Color::LightCyan,
            7 => Color::White,
            _ => Color::Reset,
        }
    } else {
        match idx {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            _ => Color::Reset,
        }
    }
}

/// Map a 256-color palette index to a ratatui color. The first 16
/// share the basic 16-color palette; the next 216 form a 6×6×6 RGB
/// cube; the final 24 are grayscale.
fn ansi_256_to_color(idx: u8) -> Color {
    if idx < 16 {
        return basic_color(idx & 0x07, idx >= 8);
    }
    if idx < 232 {
        let v = idx - 16;
        let r = (v / 36) % 6;
        let g = (v / 6) % 6;
        let b = v % 6;
        let scale = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
        return Color::Rgb(scale(r), scale(g), scale(b));
    }
    let level = 8 + (idx - 232) * 10;
    Color::Rgb(level, level, level)
}

/// UTF-8 sequence length for the leading byte. Returns 0 for
/// continuation bytes.
fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte & 0b1000_0000 == 0 {
        1
    } else if first_byte & 0b1110_0000 == 0b1100_0000 {
        2
    } else if first_byte & 0b1111_0000 == 0b1110_0000 {
        3
    } else if first_byte & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn line_has_ansi_detects_both_forms() {
        assert!(line_has_ansi("\x1b[32mhi\x1b[0m"));
        assert!(line_has_ansi("[2mfoo[0m"));
        assert!(!line_has_ansi("plain text"));
        assert!(!line_has_ansi("[link](url)"));
        assert!(!line_has_ansi("[1, 2, 3]"));
    }

    #[test]
    fn no_codes_passthrough() {
        let spans = parse_line_to_spans("hello world");
        assert_eq!(span_text(&spans), "hello world");
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn proper_sgr_with_esc_produces_styled_span() {
        let spans = parse_line_to_spans("\x1b[32mhi\x1b[0m there");
        assert_eq!(span_text(&spans), "hi there");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn bare_bracket_form_is_recognized() {
        let spans = parse_line_to_spans("[2mdim[0m bright");
        assert_eq!(span_text(&spans), "dim bright");
        assert!(spans[0].style.add_modifier.contains(Modifier::DIM));
        assert!(spans.last().is_some_and(|s| !s.style.add_modifier.contains(Modifier::DIM)));
    }

    #[test]
    fn multi_param_modifiers_accumulate() {
        let spans = parse_line_to_spans("\x1b[1;31;4mbold red ul\x1b[0m");
        let s = spans[0].style;
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(s.fg, Some(Color::Red));
    }

    #[test]
    fn rgb_color_is_parsed() {
        let spans = parse_line_to_spans("\x1b[38;2;255;100;50mhi\x1b[0m");
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(255, 100, 50)));
    }

    #[test]
    fn legit_brackets_are_preserved() {
        let spans = parse_line_to_spans("[link](url) array[0]");
        assert_eq!(span_text(&spans), "[link](url) array[0]");
    }

    #[test]
    fn real_runner_stderr_sample() {
        let s = "[2m2026-05-07T13:43:52.300628Z[0m [32m INFO[0m [2msmooth_operator_runner[0m[2m:[0m starting";
        let spans = parse_line_to_spans(s);
        let text = span_text(&spans);
        assert_eq!(text, "2026-05-07T13:43:52.300628Z  INFO smooth_operator_runner: starting");
        assert!(spans[0].style.add_modifier.contains(Modifier::DIM));
        assert!(spans.iter().any(|s| s.style.fg == Some(Color::Green) && s.content.contains("INFO")));
    }

    #[test]
    fn unicode_text_around_codes() {
        let spans = parse_line_to_spans("\x1b[32m✓\x1b[0m done — \u{2588}\u{2588}");
        assert_eq!(span_text(&spans), "✓ done — ██");
    }

    #[test]
    fn dim_then_reset_segments() {
        let spans = parse_line_to_spans("[2mDIM[0mPLAIN");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "DIM");
        assert!(spans[0].style.add_modifier.contains(Modifier::DIM));
        assert_eq!(spans[1].content, "PLAIN");
        assert!(!spans[1].style.add_modifier.contains(Modifier::DIM));
    }
}
