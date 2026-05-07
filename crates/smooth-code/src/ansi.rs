//! Strip ANSI escape sequences from text on its way into the chat.
//!
//! The runner emits structured tracing logs colored with ANSI SGR codes
//! (`\x1b[2m...\x1b[0m`, `\x1b[32m INFO`, etc.). When Big Smooth
//! forwards runner stderr as `TokenDelta` chunks for the assistant
//! message, those codes ride along — and the markdown renderer treats
//! them as plain text, leaving raw `[2m...[0m` litter all over the
//! reply.
//!
//! Strip on receipt so neither the streaming preview nor the
//! scrollback flush ever holds them. Handles two shapes:
//!
//! - **With ESC byte**: `\x1b[<params>m` — proper ANSI SGR.
//! - **Bare bracket form**: `[<digits>(;<digits>)*m` — sometimes the
//!   ESC byte is lost en route (terminal copy-paste, websocket
//!   marshalling that drops control bytes). Match the same shape and
//!   strip even without the leading ESC.
//!
//! Conservative: only strips sequences whose params are digits +
//! semicolons followed by `m` (SGR). Real chat content like markdown
//! `[link]` or array syntax stays untouched.

/// Strip ANSI SGR escape sequences from `s`, returning a new string
/// with the codes removed. Linear scan; does not allocate when no
/// codes are present (caller can skip the call entirely if it knows
/// the input is clean).
#[must_use]
pub fn strip(s: &str) -> String {
    if !s.contains('[') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // ESC `\x1b` followed by `[` — proper SGR introducer.
        if b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            if let Some(end) = scan_sgr_params(bytes, i + 2) {
                i = end + 1;
                continue;
            }
        }
        // Bare `[` followed by digits/semicolons and a closing `m`.
        // The ESC byte can be lost over WebSocket / terminal scrape.
        if b == b'[' {
            if let Some(end) = scan_sgr_params(bytes, i + 1) {
                i = end + 1;
                continue;
            }
        }
        // Not an ANSI sequence — preserve the char (UTF-8 safe: we
        // only branched on single-byte ASCII delimiters above).
        // Fast-path append a whole UTF-8 char.
        let ch_len = utf8_char_len(b);
        if ch_len == 0 {
            // Defensive: shouldn't happen on valid UTF-8, but skip
            // a stray continuation byte rather than panic.
            i += 1;
            continue;
        }
        let end = (i + ch_len).min(bytes.len());
        out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or(""));
        i = end;
    }
    out
}

/// Starting at byte index `start`, walk `digit (; digit)*` then expect
/// a final `m`. Returns the byte index of the closing `m` on success;
/// `None` if the pattern doesn't match (caller leaves the `[` in place).
fn scan_sgr_params(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    let mut saw_digit = false;
    // At least one digit must appear before the terminator. After
    // that, more digits or `;` separators are allowed; anything else
    // bails out.
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

/// UTF-8 sequence length for the leading byte. Returns 0 for
/// continuation bytes (caller should skip).
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

    #[test]
    fn no_codes_is_passthrough() {
        assert_eq!(strip("hello world"), "hello world");
        assert_eq!(strip(""), "");
    }

    #[test]
    fn strips_proper_sgr_with_esc() {
        let s = "\x1b[2mfoo\x1b[0m";
        assert_eq!(strip(s), "foo");
    }

    #[test]
    fn strips_bare_bracket_form() {
        // Same shape minus the ESC byte — what the user actually sees
        // when ESC bytes get scrubbed in transit.
        let s = "[2mfoo[0m";
        assert_eq!(strip(s), "foo");
    }

    #[test]
    fn strips_multi_param_codes() {
        assert_eq!(strip("\x1b[1;31;4mbold red underlined\x1b[0m"), "bold red underlined");
        assert_eq!(strip("[38;5;82mgreen[0m"), "green");
    }

    #[test]
    fn preserves_legit_brackets() {
        // Real markdown / array syntax that LOOKS like the bare form
        // but doesn't terminate with `m` after digits.
        assert_eq!(strip("[link](url)"), "[link](url)");
        assert_eq!(strip("array[0]"), "array[0]");
        assert_eq!(strip("[1, 2, 3]"), "[1, 2, 3]");
    }

    #[test]
    fn preserves_brackets_with_letters() {
        // `[abc]` is not a digit-seq so leave alone.
        assert_eq!(strip("[abc]"), "[abc]");
        assert_eq!(strip("[INFO]"), "[INFO]");
    }

    #[test]
    fn strips_real_runner_stderr_sample() {
        let s = "[2m2026-05-07T13:43:52.300628Z[0m [32m INFO[0m [2msmooth_operator_runner[0m[2m:[0m smooth-operator-runner starting [3moperator[0m[2m=[0mfbe0";
        let cleaned = strip(s);
        assert_eq!(cleaned, "2026-05-07T13:43:52.300628Z  INFO smooth_operator_runner: smooth-operator-runner starting operator=fbe0");
        // Make sure the literal `[2m`/`[0m` markers are gone.
        assert!(!cleaned.contains("[2m"));
        assert!(!cleaned.contains("[0m"));
    }

    #[test]
    fn preserves_unicode_text_around_codes() {
        let s = "\x1b[32m✓\x1b[0m done — \u{2588}\u{2588}";
        assert_eq!(strip(s), "✓ done — ██");
    }
}
