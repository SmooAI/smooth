//! Smoo AI branding colors and semantic style functions.
//!
//! Colors derived from `packages/ui/globals.css` in the smooai monorepo.
//! "smoo" text: gradient orange → red. "th" text: gradient green → blue.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

// ── Core brand colors (from globals.css) ──────────────────────

/// Smoo AI brand green/teal (#00a6a6) — --color-smooai-green
pub const SMOO_GREEN: Color = Color::Rgb(0, 166, 166);
/// Smoo AI brand orange (#f49f0a) — --color-smooai-orange
pub const SMOO_ORANGE: Color = Color::Rgb(244, 159, 10);
/// Smoo AI brand red (#ff6b6c) — --color-smooai-red
pub const SMOO_RED: Color = Color::Rgb(255, 107, 108);
/// Smoo AI brand blue (#bbdef0) — --color-smooai-blue
pub const SMOO_BLUE: Color = Color::Rgb(187, 222, 240);
/// Smoo AI dark blue (#020618) — --color-smooai-dark-blue
pub const SMOO_DARK_BLUE: Color = Color::Rgb(2, 6, 24);
/// Smoo AI white (#f8fafc) — --color-smooai-white
pub const SMOO_WHITE: Color = Color::Rgb(248, 250, 252);

// ── Extended palette ──────────────────────────────────────────

pub const SMOO_ORANGE_400: Color = Color::Rgb(248, 190, 87); // #f8be57
pub const SMOO_ORANGE_600: Color = Color::Rgb(200, 130, 8); // approx
pub const SMOO_RED_400: Color = Color::Rgb(255, 148, 149); // #ff9495
pub const SMOO_RED_600: Color = Color::Rgb(255, 51, 52); // #ff3334
pub const SMOO_GREEN_400: Color = Color::Rgb(74, 255, 255); // #4affff
pub const SMOO_GREEN_600: Color = Color::Rgb(0, 248, 248); // #00f8f8
pub const SMOO_BLUE_400: Color = Color::Rgb(95, 177, 220); // #5fb1dc
pub const SMOO_BLUE_600: Color = Color::Rgb(37, 122, 166); // #257aa6
pub const SMOO_GRAY_500: Color = Color::Rgb(134, 134, 134); // #868686
pub const SMOO_GRAY_700: Color = Color::Rgb(78, 78, 78); // #4e4e4e
pub const SMOO_GRAY_900: Color = Color::Rgb(29, 29, 29); // #1d1d1d

/// Muted/secondary text — --color-smooai-gray
pub const MUTED: Color = Color::Rgb(163, 163, 163);
/// Error indicator — --color-smooai-red
pub const ERROR_RED: Color = Color::Rgb(255, 107, 108);
/// Success indicator
pub const SUCCESS_GREEN: Color = Color::Rgb(46, 160, 67);

// ── Gradient title spans ──────────────────────────────────────

/// "smoo" gradient: orange → red
pub fn smoo_gradient() -> Vec<Span<'static>> {
    vec![
        Span::styled("s", Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)),
        Span::styled("m", Style::default().fg(Color::Rgb(248, 140, 40)).add_modifier(Modifier::BOLD)),
        Span::styled("o", Style::default().fg(Color::Rgb(252, 120, 70)).add_modifier(Modifier::BOLD)),
        Span::styled("o", Style::default().fg(SMOO_RED).add_modifier(Modifier::BOLD)),
    ]
}

/// "th" gradient: green → blue
pub fn th_gradient() -> Vec<Span<'static>> {
    vec![
        Span::styled("t", Style::default().fg(SMOO_GREEN).add_modifier(Modifier::BOLD)),
        Span::styled("h", Style::default().fg(SMOO_BLUE_400).add_modifier(Modifier::BOLD)),
    ]
}

/// Full branded title: "th" (green→blue) + " " + "smoo" (orange→red)
pub fn branded_title() -> Vec<Span<'static>> {
    let mut spans = th_gradient();
    spans.push(Span::raw(" "));
    spans.extend(smoo_gradient());
    spans
}

/// The full "Smooth" wordmark as a sequence of ratatui spans with the
/// same per-character gradient the CLI uses for `th`'s own banner
/// (see `crates/smooth-cli/src/gradient.rs::smooth()`):
///
///   S m o o  →  #f49f0a (orange) → #ff6b6c (pink), linear over 4 chars
///   t h      →  #00a6a6 (teal)   → #1238dd (blue), linear over 2 chars
///
/// Use anywhere the TUI prints "Smooth" so it reads the way the brand
/// reads elsewhere in the product.
pub fn smooth_wordmark() -> Vec<Span<'static>> {
    const SMOO_START: (u8, u8, u8) = (0xf4, 0x9f, 0x0a);
    const SMOO_END: (u8, u8, u8) = (0xff, 0x6b, 0x6c);
    const TH_START: (u8, u8, u8) = (0x00, 0xa6, 0xa6);
    const TH_END: (u8, u8, u8) = (0x12, 0x38, 0xdd);

    fn spans(text: &str, start: (u8, u8, u8), end: (u8, u8, u8)) -> Vec<Span<'static>> {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        chars
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let t = if n <= 1 { 0.0 } else { i as f64 / (n - 1) as f64 };
                let r = lerp_u8(start.0, end.0, t);
                let g = lerp_u8(start.1, end.1, t);
                let b = lerp_u8(start.2, end.2, t);
                Span::styled(c.to_string(), Style::default().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD))
            })
            .collect()
    }

    let mut out = spans("Smoo", SMOO_START, SMOO_END);
    out.extend(spans("th", TH_START, TH_END));
    out
}

/// Style for the main title bar — orange is the brand's primary
/// accent; green is secondary and shows up on assistant labels and
/// the vertical banner gradient.
pub fn title() -> Style {
    Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)
}

/// Style for user message labels ("You").
pub fn user_label() -> Style {
    Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)
}

/// Style for assistant message labels ("Smooth").
pub fn assistant_label() -> Style {
    Style::default().fg(SMOO_GREEN).add_modifier(Modifier::BOLD)
}

/// Style for the input text area.
pub fn input_style() -> Style {
    Style::default().fg(Color::White)
}

/// Style for the status bar.
pub fn status_style() -> Style {
    Style::default().fg(MUTED)
}

/// Style for muted/secondary text.
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

/// Style for error text.
pub fn error() -> Style {
    Style::default().fg(ERROR_RED)
}

/// Style for success text.
pub fn success() -> Style {
    Style::default().fg(SUCCESS_GREEN)
}

// ── Gradient and dynamic color helpers ───────────────────────

/// Interpolate between `SMOO_ORANGE` and `SMOO_GREEN` based on row position.
///
/// `row` 0 returns pure orange, `row == total - 1` returns pure green.
/// `total` must be >= 1; if 1, returns orange.
pub fn gradient_row(row: usize, total: usize) -> Style {
    let total = total.max(1);
    let t = if total <= 1 { 0.0 } else { row as f64 / (total as f64 - 1.0) };

    // SMOO_ORANGE = (244, 159, 10), SMOO_GREEN = (0, 166, 166)
    let r = lerp_u8(244, 0, t);
    let g = lerp_u8(159, 166, t);
    let b = lerp_u8(10, 166, t);

    Style::default().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD)
}

/// Color for column `i` of a `total`-wide rendering of the **Smoo**
/// half of the wordmark — the orange→coral→pink gradient from
/// `crates/smooth-web/web/public/logo.svg`:
///
///   offset 0.00..0.30 → solid orange (#f49f0a)
///   offset 0.30..0.79 → lerp orange  → coral (#fb7a4d)
///   offset 0.79..1.00 → lerp coral   → pink  (#ff6b6c)
///
/// The 30 % solid leading band comes from the SVG `<stop offset>`
/// values; without it the gradient looks washed-out.
#[must_use]
pub fn smoo_gradient_color(i: usize, total: usize) -> Color {
    const STOP_0: (u8, u8, u8) = (0xf4, 0x9f, 0x0a); // orange
    const STOP_1: (u8, u8, u8) = (0xfb, 0x7a, 0x4d); // coral
    const STOP_2: (u8, u8, u8) = (0xff, 0x6b, 0x6c); // pink

    let total = total.max(1);
    let t = i as f64 / (total - 1).max(1) as f64;
    let (r, g, b) = if t <= 0.30 {
        STOP_0
    } else if t < 0.79 {
        let u = (t - 0.30) / (0.79 - 0.30);
        (lerp_u8(STOP_0.0, STOP_1.0, u), lerp_u8(STOP_0.1, STOP_1.1, u), lerp_u8(STOP_0.2, STOP_1.2, u))
    } else {
        let u = (t - 0.79) / (1.0 - 0.79);
        (lerp_u8(STOP_1.0, STOP_2.0, u), lerp_u8(STOP_1.1, STOP_2.1, u), lerp_u8(STOP_1.2, STOP_2.2, u))
    };
    Color::Rgb(r, g, b)
}

/// Color for column `i` of a `total`-wide rendering of the **th**
/// half of the wordmark — the teal→blue gradient from
/// `crates/smooth-web/web/public/logo.svg`:
///
///   offset 0.00..0.43 → solid teal (#00a6a6)
///   offset 0.43..1.00 → lerp teal  → blue (#1238dd)
#[must_use]
pub fn th_gradient_color(i: usize, total: usize) -> Color {
    const STOP_0: (u8, u8, u8) = (0x00, 0xa6, 0xa6); // teal
    const STOP_1: (u8, u8, u8) = (0x12, 0x38, 0xdd); // blue

    let total = total.max(1);
    let t = i as f64 / (total - 1).max(1) as f64;
    let (r, g, b) = if t <= 0.43 {
        STOP_0
    } else {
        let u = (t - 0.43) / (1.0 - 0.43);
        (lerp_u8(STOP_0.0, STOP_1.0, u), lerp_u8(STOP_0.1, STOP_1.1, u), lerp_u8(STOP_0.2, STOP_1.2, u))
    };
    Color::Rgb(r, g, b)
}

/// Return a color for a file based on its extension.
pub fn file_color(extension: &str) -> Color {
    match extension {
        "rs" => SMOO_ORANGE,
        "ts" | "tsx" | "js" | "jsx" => SMOO_BLUE_400,
        "md" => SMOO_GREEN,
        "json" => Color::Rgb(255, 255, 100),                  // yellow
        "toml" | "yaml" | "yml" => Color::Rgb(100, 220, 220), // cyan
        _ => Color::White,
    }
}

/// Style for a tool-call status border.
pub fn tool_status_border(status: crate::state::ToolStatus) -> Style {
    use crate::state::ToolStatus;
    match status {
        ToolStatus::Pending => Style::default().fg(MUTED),
        ToolStatus::Running => Style::default().fg(SMOO_ORANGE),
        ToolStatus::Done => Style::default().fg(SUCCESS_GREEN),
        ToolStatus::Error => Style::default().fg(ERROR_RED),
    }
}

/// Panel border style — bright orange when focused (the brand's
/// primary accent), dim gray when inactive. Green is reserved for
/// assistant labels + the banner gradient so users can visually
/// separate "where the UI wants attention" (orange) from
/// "agent speaking" (green).
pub fn panel_border(active: bool) -> Style {
    if active {
        Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SMOO_GRAY_700)
    }
}

/// Border for the message-input panel. Always the primary accent
/// (orange + bold) so the user can find "where do I type" at a
/// glance — even when the chat panel is the focused one. Falls back
/// to muted gray when the user has explicitly escaped into normal
/// mode.
pub fn input_border(mode: crate::state::Mode) -> Style {
    match mode {
        crate::state::Mode::Input => Style::default().fg(SMOO_ORANGE).add_modifier(Modifier::BOLD),
        crate::state::Mode::Normal => Style::default().fg(SMOO_GRAY_700),
    }
}

/// Linear interpolation between two u8 values.
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    let result = f64::from(a) + (f64::from(b) - f64::from(a)) * t;
    result.round().clamp(0.0, 255.0) as u8
}

// ── Flow signature + glyph vocabulary (the glow-up) ──────────────
//
// "Smooth" is a color that flows warm → cool. That flow is the brand's
// signature; the chrome here makes it literal.

/// The four brand stops, warm → cool: orange → pink → teal → blue — the
/// wordmark's colors in logo order.
const FLOW_STOPS: [(u8, u8, u8); 4] = [(0xf4, 0x9f, 0x0a), (0xff, 0x6b, 0x6c), (0x00, 0xa6, 0xa6), (0x12, 0x38, 0xdd)];

/// Interpolate the full warm→cool brand gradient at `t` ∈ [0,1] (three even
/// segments across the four [`FLOW_STOPS`]).
#[must_use]
pub fn flow_color(t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let segs = (FLOW_STOPS.len() - 1) as f64;
    let scaled = t * segs;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i = (scaled.floor() as usize).min(FLOW_STOPS.len() - 2);
    let local = scaled - i as f64;
    let (a, b) = (FLOW_STOPS[i], FLOW_STOPS[i + 1]);
    Color::Rgb(lerp_u8(a.0, b.0, local), lerp_u8(a.1, b.1, local), lerp_u8(a.2, b.2, local))
}

/// **The signature chrome.** A `width`-cell horizontal rule whose every cell
/// steps along the full Smooth gradient (warm→cool) — the brand flowing across
/// the divider. Reserve it for the header underline so it reads as special.
/// `ch` is the rule glyph (e.g. `'─'`).
#[must_use]
pub fn flow_rule(width: usize, ch: char) -> Vec<Span<'static>> {
    (0..width)
        .map(|cell| {
            let t = if width <= 1 { 0.0 } else { cell as f64 / (width - 1) as f64 };
            Span::styled(ch.to_string(), Style::default().fg(flow_color(t)))
        })
        .collect()
}

// ── Glyph vocabulary — one curated set, used everywhere ──────────

/// Prompt chevron — the user's turn (warm accent).
pub const GLYPH_USER: &str = "❯";
/// The spark — the agent's turn (cool accent).
pub const GLYPH_ASSISTANT: &str = "✦";
/// A tool invocation (mist).
pub const GLYPH_TOOL: &str = "▸";
/// A tool/step succeeded (cool).
pub const GLYPH_OK: &str = "✓";
/// A tool/step failed or was blocked (warm-end / pink).
pub const GLYPH_ERR: &str = "✗";
/// A system / meta line (mist).
pub const GLYPH_SYSTEM: &str = "·";
/// The streaming-output cursor.
pub const GLYPH_CURSOR: &str = "▌";

/// The agent label glyph + its cool accent, ready to drop into a line.
pub fn assistant_glyph() -> Span<'static> {
    Span::styled(GLYPH_ASSISTANT, assistant_label())
}

/// The user prompt glyph + its warm accent.
pub fn user_glyph() -> Span<'static> {
    Span::styled(GLYPH_USER, user_label())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_constants_exist() {
        assert_eq!(SMOO_GREEN, Color::Rgb(0, 166, 166));
        assert_eq!(SMOO_ORANGE, Color::Rgb(244, 159, 10));
        assert_eq!(SMOO_RED, Color::Rgb(255, 107, 108));
        assert_eq!(SMOO_BLUE, Color::Rgb(187, 222, 240));
        assert_eq!(SMOO_DARK_BLUE, Color::Rgb(2, 6, 24));
        assert_eq!(SMOO_WHITE, Color::Rgb(248, 250, 252));
        assert_eq!(MUTED, Color::Rgb(163, 163, 163));
        assert_eq!(ERROR_RED, Color::Rgb(255, 107, 108));
        assert_eq!(SUCCESS_GREEN, Color::Rgb(46, 160, 67));
    }

    #[test]
    fn test_smoo_gradient_has_4_chars() {
        let spans = smoo_gradient();
        assert_eq!(spans.len(), 4);
    }

    #[test]
    fn test_th_gradient_has_2_chars() {
        let spans = th_gradient();
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn test_branded_title() {
        let spans = branded_title();
        assert_eq!(spans.len(), 7); // t, h, " ", s, m, o, o
    }

    #[test]
    fn flow_color_runs_warm_to_cool() {
        assert_eq!(flow_color(0.0), Color::Rgb(0xf4, 0x9f, 0x0a)); // warm orange
        assert_eq!(flow_color(1.0), Color::Rgb(0x12, 0x38, 0xdd)); // cool blue
                                                                   // The midpoint sits in the pink→teal segment (a blend, not a stop).
        let Color::Rgb(r, _, b) = flow_color(0.5) else { panic!("rgb") };
        assert!(r < 0xff && b > 0x00, "midpoint blends warm→cool: {r},{b}");
    }

    #[test]
    fn flow_rule_has_one_span_per_cell() {
        assert_eq!(flow_rule(40, '─').len(), 40);
        assert!(flow_rule(0, '─').is_empty());
    }

    #[test]
    fn test_style_functions_return_styles() {
        // Ensure style functions don't panic and return non-default styles
        let t = title();
        assert_eq!(t.fg, Some(SMOO_ORANGE));

        let ul = user_label();
        assert_eq!(ul.fg, Some(SMOO_ORANGE));

        let al = assistant_label();
        assert_eq!(al.fg, Some(SMOO_GREEN));

        let is = input_style();
        assert_eq!(is.fg, Some(Color::White));

        let ss = status_style();
        assert_eq!(ss.fg, Some(MUTED));
    }

    #[test]
    fn test_gradient_row_interpolates_correctly() {
        // First row = pure SMOO_ORANGE
        let first = gradient_row(0, 6);
        assert_eq!(first.fg, Some(Color::Rgb(244, 159, 10)));

        // Last row = pure SMOO_GREEN
        let last = gradient_row(5, 6);
        assert_eq!(last.fg, Some(Color::Rgb(0, 166, 166)));

        // Middle row should be somewhere between
        let mid = gradient_row(3, 6);
        if let Some(Color::Rgb(r, g, b)) = mid.fg {
            assert!(r < 244, "mid red should be less than orange red");
            assert!(r > 0, "mid red should be greater than green red");
            assert!(b > 10, "mid blue should be greater than orange blue");
            assert!(b < 166, "mid blue should be less than green blue");
            // green channel stays close (159 -> 166)
            assert!(g >= 159);
            assert!(g <= 166);
        } else {
            panic!("expected Rgb color");
        }

        // Edge case: total=1 returns orange
        let single = gradient_row(0, 1);
        assert_eq!(single.fg, Some(Color::Rgb(244, 159, 10)));
    }

    #[test]
    fn test_file_color_returns_different_colors() {
        let rs = file_color("rs");
        let ts = file_color("ts");
        let md = file_color("md");
        let json = file_color("json");
        let toml = file_color("toml");
        let other = file_color("xyz");

        assert_eq!(rs, SMOO_ORANGE);
        assert_eq!(ts, SMOO_BLUE_400);
        assert_eq!(md, SMOO_GREEN);
        // Ensure json/toml/other are all distinct
        assert_ne!(json, toml);
        assert_ne!(json, other);
        assert_eq!(other, Color::White);
    }

    #[test]
    fn test_tool_status_border_returns_correct_colors() {
        use crate::state::ToolStatus;

        let pending = tool_status_border(ToolStatus::Pending);
        assert_eq!(pending.fg, Some(MUTED));

        let running = tool_status_border(ToolStatus::Running);
        assert_eq!(running.fg, Some(SMOO_ORANGE));

        let done = tool_status_border(ToolStatus::Done);
        assert_eq!(done.fg, Some(SUCCESS_GREEN));

        let error = tool_status_border(ToolStatus::Error);
        assert_eq!(error.fg, Some(ERROR_RED));
    }

    #[test]
    fn test_panel_border_active_vs_inactive() {
        let active = panel_border(true);
        let inactive = panel_border(false);

        assert_ne!(active.fg, inactive.fg);
        assert_eq!(active.fg, Some(SMOO_ORANGE));
        assert_eq!(inactive.fg, Some(SMOO_GRAY_700));
    }

    #[test]
    fn test_input_border_is_orange_in_input_mode_gray_in_normal() {
        use crate::state::Mode;
        assert_eq!(input_border(Mode::Input).fg, Some(SMOO_ORANGE));
        assert_eq!(input_border(Mode::Normal).fg, Some(SMOO_GRAY_700));
    }
}
