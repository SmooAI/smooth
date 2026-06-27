//! Wordmark gradients — apply the Smooth brand colors per-character via
//! ANSI 24-bit truecolor escapes.
//!
//! Used anywhere the CLI prints "Smooth" or "Smoo AI" so the wordmark
//! carries the same gradient as the logo:
//!
//!   "Smoo"  →  #f49f0a (orange) → #ff6b6c (pink)
//!   "th"    →  #00a6a6 (teal)   → #1238dd (blue)
//!
//! The combined `smooth()` helper stitches the two halves so the full
//! word matches the horizontal logo.

const SMOO_START: (u8, u8, u8) = (0xf4, 0x9f, 0x0a);
const SMOO_END: (u8, u8, u8) = (0xff, 0x6b, 0x6c);
const TH_START: (u8, u8, u8) = (0x00, 0xa6, 0xa6);
const TH_END: (u8, u8, u8) = (0x12, 0x38, 0xdd);

/// Return `text` with a per-character RGB gradient from `start` to `end`.
/// Always bold — the word is a brand mark, not prose.
fn gradient(text: &str, start: (u8, u8, u8), end: (u8, u8, u8)) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let n = chars.len();
    let mut out = String::with_capacity(text.len() * 20);
    out.push_str("\x1b[1m"); // bold on
    for (i, c) in chars.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let t = if n == 1 { 0.0f32 } else { (i as f32) / ((n - 1) as f32) };
        let r = lerp(start.0, end.0, t);
        let g = lerp(start.1, end.1, t);
        let b = lerp(start.2, end.2, t);
        out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{c}"));
    }
    out.push_str("\x1b[0m"); // reset
    out
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn lerp(a: u8, b: u8, t: f32) -> u8 {
    let af = f32::from(a);
    let bf = f32::from(b);
    (af + (bf - af) * t).round().clamp(0.0, 255.0) as u8
}

/// Apply the "Smoo"-half gradient (orange → pink). Also correct for
/// the "Smoo" in "Smoo AI".
#[must_use]
pub fn smoo(text: &str) -> String {
    gradient(text, SMOO_START, SMOO_END)
}

/// Apply the "th"-half gradient (teal → blue).
#[must_use]
pub fn th(text: &str) -> String {
    gradient(text, TH_START, TH_END)
}

/// The full wordmark "Smooth" — "Smoo" + "th" with their respective
/// gradients stitched together. Matches the horizontal logo.
#[must_use]
pub fn smooth() -> String {
    format!("{}{}", smoo("Smoo"), th("th"))
}

/// "Smoo AI" wordmark — "Smoo" gradient on the Smoo, " AI" plain.
/// The AI in the logo isn't itself a gradient mark, so we leave it
/// to the caller's own styling.
#[must_use]
pub fn smoo_ai() -> String {
    format!("{} AI", smoo("Smoo"))
}

/// The four brand stops, warm → cool: orange → pink → teal → blue — the same
/// colors the wordmark uses, in the order the logo reads.
const FLOW_STOPS: [(u8, u8, u8); 4] = [SMOO_START, SMOO_END, TH_START, TH_END];

/// Interpolate the full warm→cool brand gradient at `t` ∈ [0,1] across the four
/// [`FLOW_STOPS`] (three even segments).
#[must_use]
pub fn flow_color(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let segs = (FLOW_STOPS.len() - 1) as f32;
    let scaled = t * segs;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i = (scaled.floor() as usize).min(FLOW_STOPS.len() - 2);
    let local = scaled - i as f32;
    let (a, b) = (FLOW_STOPS[i], FLOW_STOPS[i + 1]);
    (lerp(a.0, b.0, local), lerp(a.1, b.1, local), lerp(a.2, b.2, local))
}

/// **The signature chrome.** A horizontal rule `width` cells wide whose every
/// cell steps along the full Smooth gradient (warm → cool) — the brand
/// wordmark's flow stretched into a line. Use it for headers/dividers so the
/// chrome itself reads as "smooth". `ch` is the rule glyph (e.g. `'─'`).
#[must_use]
pub fn flow_rule(width: usize, ch: char) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(width * 20 + 4);
    for cell in 0..width {
        #[allow(clippy::cast_precision_loss)]
        let t = if width == 1 { 0.0 } else { cell as f32 / (width - 1) as f32 };
        let (r, g, b) = flow_color(t);
        out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}"));
    }
    out.push_str("\x1b[0m");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_emits_truecolor_for_every_char() {
        let out = gradient("abcd", SMOO_START, SMOO_END);
        // four characters → four truecolor escapes
        assert_eq!(out.matches("\x1b[38;2;").count(), 4);
        assert!(out.starts_with("\x1b[1m"));
        assert!(out.ends_with("\x1b[0m"));
        assert!(out.contains('a'));
        assert!(out.contains('d'));
    }

    #[test]
    fn gradient_empty_is_empty() {
        assert_eq!(gradient("", SMOO_START, SMOO_END), "");
    }

    #[test]
    fn smooth_stitches_halves() {
        let out = smooth();
        assert!(out.contains('S'));
        assert!(out.contains('m'));
        assert!(out.contains('o'));
        assert!(out.contains('t'));
        assert!(out.contains('h'));
        // 4 (Smoo) + 2 (th) = 6 color escapes
        assert_eq!(out.matches("\x1b[38;2;").count(), 6);
    }

    #[test]
    fn lerp_interpolates() {
        assert_eq!(lerp(0, 100, 0.0), 0);
        assert_eq!(lerp(0, 100, 1.0), 100);
        assert_eq!(lerp(0, 100, 0.5), 50);
    }

    #[test]
    fn flow_color_runs_warm_to_cool() {
        assert_eq!(flow_color(0.0), SMOO_START); // warm orange
        assert_eq!(flow_color(1.0), TH_END); // cool blue
    }

    #[test]
    fn flow_rule_colors_every_cell_and_resets() {
        let out = flow_rule(24, '─');
        assert_eq!(out.matches("\x1b[38;2;").count(), 24); // one truecolor per cell
        assert_eq!(out.matches('─').count(), 24);
        assert!(out.ends_with("\x1b[0m"));
        assert_eq!(flow_rule(0, '─'), "");
    }
}
