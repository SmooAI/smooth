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

/// Big Smooth's dark accessories — the fedora and sunglasses from the web
/// avatar (`BigSmoothFace.tsx` lensMat/hatMat, ~#080812). Dark-on-gradient
/// reads as shades on ANY terminal ground because the bright head surrounds
/// it, exactly like the web face on its dark canvas (pearl th-a67752).
pub const FACE_DARK: Color = Color::Rgb(0x08, 0x08, 0x12);

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
/// Success indicator.
///
/// Deliberately NOT a teal-adjacent green: the teal end of the `th`
/// gradient is Big Smooth's face, and a success tick that reads as "the
/// agent is here" is exactly the collision Presence forbids.
pub const SUCCESS_GREEN: Color = Color::Rgb(111, 207, 151);

// ── Presence semantics ────────────────────────────────────────
//
// Presence spends color on PRESENCE and ATTENTION, never on decoration.
// The palette above is the raw material; these four are the meanings.
// Before adding a colour here, ask which of the four it is — if it is
// none of them, it does not get a colour at all.

/// The `th` face, teal end. Big Smooth's PRESENCE — his mark, his turns,
/// his heartbeat. Chrome never wears the face.
pub const TH_TEAL: Color = Color::Rgb(0x00, 0xa6, 0xa6);
/// The `th` face, blue end.
pub const TH_BLUE: Color = Color::Rgb(0x12, 0x38, 0xdd);

/// ATTENTION — the affirmative action, and where the user acts. This is
/// the primary accent that used to be spread across focus, typing, and
/// running tools all at once.
pub const CORAL: Color = Color::Rgb(0xfb, 0x7a, 0x4d);

/// ATTENTION — "Big Smooth needs you", and nothing else. An approval
/// gate, a blocked run, a question he cannot answer alone. The moment
/// amber shows up anywhere else it stops meaning anything, so there is
/// exactly one style function that returns it: [`needs_you`].
pub const AMBER: Color = Color::Rgb(0xf4, 0x9f, 0x0a);

/// Awake / alive status.
pub const ONLINE: Color = Color::Rgb(0x6f, 0xcf, 0x97);

/// Warm hairline for chrome that must recede — panel borders, rules,
/// separators. Warm, because Presence's ground is a room you share, not
/// a dashboard you audit; cool grey is Aurora's, not ours.
pub const HAIRLINE: Color = Color::Rgb(0x4a, 0x45, 0x42);
/// Warm hairline, raised — the FOCUSED panel's border. Brighter neutral
/// plus BOLD, not an accent: focus is a change in weight, not a change
/// in meaning.
pub const HAIRLINE_BRIGHT: Color = Color::Rgb(0x8a, 0x82, 0x7c);

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

/// Style for titles and section labels.
///
/// Warm off-white + bold, NOT the brand orange it used to be. Brand
/// orange is byte-identical to [`AMBER`] (`#f49f0a`), so every title and
/// every panel border wearing it meant amber could never be reserved for
/// "Big Smooth needs you" — the one thing it is supposed to mean. Titles
/// are chrome; chrome gets weight, not colour.
pub fn title() -> Style {
    Style::default().fg(SMOO_WHITE).add_modifier(Modifier::BOLD)
}

/// Style for user message labels ("You").
///
/// Deliberately quiet. The user is not a presence indicator and not an
/// attention state — and keeping it neutral is what lets the assistant's
/// teal→blue face read instantly as "he is talking now".
pub fn user_label() -> Style {
    Style::default().fg(SMOO_WHITE).add_modifier(Modifier::BOLD)
}

/// Style for assistant message labels ("Smooth").
///
/// This is Big Smooth speaking, so it wears the face. Use
/// [`assistant_label_spans`] where the label is long enough for the
/// gradient to read; this flat form is the fallback for one- or
/// two-character marks.
pub fn assistant_label() -> Style {
    Style::default().fg(TH_TEAL).add_modifier(Modifier::BOLD)
}

/// The assistant's label rendered in the `th` face gradient — teal→blue
/// across the word. Presence's one rule: the face marks where *he* is,
/// and nothing else in the UI may wear it.
pub fn assistant_label_spans(label: &str) -> Vec<Span<'static>> {
    let n = label.chars().count();
    label
        .chars()
        .enumerate()
        .map(|(i, c)| Span::styled(c.to_string(), Style::default().fg(th_gradient_color(i, n)).add_modifier(Modifier::BOLD)))
        .collect()
}

/// "Big Smooth needs you" — an approval gate, a blocked run, a question
/// only the user can answer. The ONLY function that returns [`AMBER`].
/// If you are reaching for this for anything else, reach for
/// [`attention`] instead.
pub fn needs_you() -> Style {
    Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
}

/// The affirmative action / where the user acts.
pub fn attention() -> Style {
    Style::default().fg(CORAL).add_modifier(Modifier::BOLD)
}

/// Awake / alive.
pub fn online() -> Style {
    Style::default().fg(ONLINE)
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

/// A tool call's status as a GLYPH.
///
/// Presence rule: state is encoded in form, not only in colour. Roughly
/// 8% of users cannot read a hue distinction, `NO_COLOR` strips it
/// entirely, and a piped capture has none at all — the glyph is what
/// survives all three.
#[must_use]
pub const fn tool_status_glyph(status: crate::state::ToolStatus) -> &'static str {
    use crate::state::ToolStatus;
    match status {
        ToolStatus::Pending => "○",
        ToolStatus::Running => "◐",
        ToolStatus::Done => "●",
        ToolStatus::Error => "✗",
    }
}

/// Style for a tool-call status border.
pub fn tool_status_border(status: crate::state::ToolStatus) -> Style {
    use crate::state::ToolStatus;
    match status {
        ToolStatus::Pending => Style::default().fg(MUTED),
        ToolStatus::Running => Style::default().fg(CORAL),
        ToolStatus::Done => Style::default().fg(SUCCESS_GREEN),
        ToolStatus::Error => Style::default().fg(ERROR_RED),
    }
}

/// Panel border style — a warm hairline that recedes, brighter and bold
/// when focused.
///
/// Chrome does not get an accent colour. It used to be brand orange,
/// which meant orange simultaneously said "this panel is focused",
/// "type here", and "a tool is running" — three meanings, so none of
/// them read. Focus is now carried by WEIGHT (brighter + bold), leaving
/// coral free to mean "act here" and amber free to mean "he needs you".
pub fn panel_border(active: bool) -> Style {
    if active {
        Style::default().fg(HAIRLINE_BRIGHT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(HAIRLINE)
    }
}

/// Border for the message-input panel. Always the action accent
/// (coral + bold) so the user can find "where do I type" at a
/// glance — even when the chat panel is the focused one. Falls back
/// to muted gray when the user has explicitly escaped into normal
/// mode.
pub fn input_border(mode: crate::state::Mode) -> Style {
    match mode {
        crate::state::Mode::Input => Style::default().fg(CORAL).add_modifier(Modifier::BOLD),
        crate::state::Mode::Normal => Style::default().fg(HAIRLINE),
    }
}

/// Linear interpolation between two u8 values.
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    let result = f64::from(a) + (f64::from(b) - f64::from(a)) * t;
    result.round().clamp(0.0, 255.0) as u8
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
        assert_eq!(SUCCESS_GREEN, Color::Rgb(111, 207, 151));
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
    fn test_style_functions_return_styles() {
        // Ensure style functions don't panic and return non-default styles
        let t = title();
        assert_eq!(t.fg, Some(SMOO_WHITE));

        let ul = user_label();
        assert_eq!(ul.fg, Some(SMOO_WHITE));

        let al = assistant_label();
        assert_eq!(al.fg, Some(TH_TEAL));

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
        assert_eq!(running.fg, Some(CORAL));

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
        assert_eq!(active.fg, Some(HAIRLINE_BRIGHT));
        assert_eq!(inactive.fg, Some(HAIRLINE));
        // Focus is carried by WEIGHT, so it survives NO_COLOR.
        assert!(active.add_modifier.contains(Modifier::BOLD));
    }

    /// Presence's load-bearing rule: chrome never wears an accent. If a
    /// panel border ever becomes coral/amber/face-coloured again, those
    /// colours stop meaning "act here" / "he needs you" / "he is here".
    #[test]
    fn chrome_never_wears_an_accent_colour() {
        for style in [panel_border(true), panel_border(false)] {
            let fg = style.fg.expect("border has a colour");
            assert!(
                ![CORAL, AMBER, TH_TEAL, TH_BLUE, SMOO_ORANGE, ONLINE].contains(&fg),
                "panel chrome must stay a neutral hairline, got {fg:?}"
            );
        }
    }

    /// Amber has exactly one meaning, so exactly one style function may
    /// return it. This test is the enforcement.
    #[test]
    fn amber_is_only_ever_needs_you() {
        assert_eq!(needs_you().fg, Some(AMBER));
        for style in [
            title(),
            user_label(),
            attention(),
            online(),
            assistant_label(),
            panel_border(true),
            panel_border(false),
            input_border(crate::state::Mode::Input),
            input_border(crate::state::Mode::Normal),
            muted(),
            error(),
            success(),
            status_style(),
        ] {
            assert_ne!(style.fg, Some(AMBER), "only needs_you() may return amber");
        }
    }

    /// The face marks Big Smooth's presence and nothing else.
    #[test]
    fn only_the_assistant_wears_the_face() {
        assert_eq!(assistant_label().fg, Some(TH_TEAL));
        for style in [
            panel_border(true),
            panel_border(false),
            input_border(crate::state::Mode::Input),
            attention(),
            needs_you(),
        ] {
            let fg = style.fg.expect("has a colour");
            assert!(![TH_TEAL, TH_BLUE].contains(&fg), "chrome must not wear the face, got {fg:?}");
        }
    }

    #[test]
    fn assistant_label_spans_run_teal_to_blue() {
        let spans = assistant_label_spans("Smooth");
        assert_eq!(spans.len(), 6);
        assert_eq!(spans[0].style.fg, Some(TH_TEAL), "starts at the teal end");
        assert_eq!(spans[5].style.fg, Some(TH_BLUE), "ends at the blue end");
        // Degenerate widths must not panic or divide by zero.
        assert_eq!(assistant_label_spans("").len(), 0);
        assert_eq!(assistant_label_spans("t").len(), 1);
    }

    /// Colour is never the only carrier: every status has a distinct glyph.
    #[test]
    fn every_tool_status_has_a_distinct_glyph() {
        use crate::state::ToolStatus;
        let glyphs: Vec<&str> = [ToolStatus::Pending, ToolStatus::Running, ToolStatus::Done, ToolStatus::Error]
            .into_iter()
            .map(tool_status_glyph)
            .collect();
        let mut sorted = glyphs.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), glyphs.len(), "statuses must be distinguishable without colour: {glyphs:?}");
        // Single-cell glyphs only — emoji wreck column alignment.
        for g in glyphs {
            assert_eq!(g.chars().count(), 1, "status glyph {g:?} must be one character");
        }
    }

    #[test]
    fn test_input_border_is_the_action_accent_in_input_mode() {
        use crate::state::Mode;
        assert_eq!(input_border(Mode::Input).fg, Some(CORAL));
        assert_eq!(input_border(Mode::Normal).fg, Some(HAIRLINE));
    }
}
