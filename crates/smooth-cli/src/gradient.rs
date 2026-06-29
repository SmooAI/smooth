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
}
