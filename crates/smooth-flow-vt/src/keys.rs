//! tmux key names → logical key presses for libghostty-vt's key encoder.
//!
//! SmoothFlow manifests (`crates/smooth-flow/harnesses/*.toml`), `harness.rs`
//! defaults and the engine name keys the way `tmux send-keys` does: `Enter`,
//! `Escape`, `C-c`, `Down`, `y`, `1`. Under tmux, tmux turned those into bytes
//! for the pane's current modes. Without tmux this table does the naming half
//! and libghostty-vt's encoder does the bytes, so DECCKM, Kitty keyboard flags
//! and modifyOtherKeys are honoured exactly as a real terminal would.
//!
//! Grammar (tmux's): any number of `C-`, `M-` and `S-` prefixes (also `^x`
//! for `C-x`), then a named key or one character. Named keys match
//! case-insensitively, as in tmux; a single character is taken literally.

use crate::ffi::mods;

/// A parsed key: the bridge's logical key name, modifiers and the text it types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    /// A name from `csrc/flow_vt.c`'s `KEYS` table ("" = unidentified, text only).
    pub key: &'static str,
    /// `ffi::mods` bits.
    pub mods: u32,
    /// The unmodified text the key types, if it types any.
    pub text: Option<String>,
}

/// tmux's names for non-printing keys, with their aliases.
const NAMED: &[(&[&str], &str, Option<&str>)] = &[
    (&["Enter"], "enter", None),
    (&["Escape", "Esc"], "escape", None),
    (&["Tab"], "tab", None),
    (&["BSpace"], "backspace", None),
    (&["Space"], "space", Some(" ")),
    (&["Up"], "up", None),
    (&["Down"], "down", None),
    (&["Left"], "left", None),
    (&["Right"], "right", None),
    (&["Home"], "home", None),
    (&["End"], "end", None),
    (&["PageUp", "PgUp", "PPage"], "page_up", None),
    (&["PageDown", "PgDn", "NPage"], "page_down", None),
    (&["Insert", "IC"], "insert", None),
    (&["Delete", "DC"], "delete", None),
    (&["F1"], "f1", None),
    (&["F2"], "f2", None),
    (&["F3"], "f3", None),
    (&["F4"], "f4", None),
    (&["F5"], "f5", None),
    (&["F6"], "f6", None),
    (&["F7"], "f7", None),
    (&["F8"], "f8", None),
    (&["F9"], "f9", None),
    (&["F10"], "f10", None),
    (&["F11"], "f11", None),
    (&["F12"], "f12", None),
];

/// Characters with a physical key of their own on a US layout (the bridge's
/// `KEYS` table), so Kitty mode can report the key, not just the text.
fn physical(c: char) -> Option<&'static str> {
    const KEYS: &[(char, &str)] = &[
        ('`', "`"),
        ('\\', "\\"),
        ('[', "["),
        (']', "]"),
        (',', ","),
        ('=', "="),
        ('-', "-"),
        ('.', "."),
        ('\'', "'"),
        (';', ";"),
        ('/', "/"),
    ];
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    let lower = c.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        return Some(LETTERS[(lower as u8 - b'a') as usize]);
    }
    if c.is_ascii_digit() {
        return Some(DIGITS[(c as u8 - b'0') as usize]);
    }
    KEYS.iter().find(|(k, _)| *k == c).map(|(_, n)| *n)
}

/// Parse a tmux key name. `None` for an empty name, a dangling modifier
/// (`C-`), an unknown multi-character name, or a control character (send
/// those as text, not as keys).
pub fn parse(name: &str) -> Option<KeyPress> {
    let mut rest = name;
    let mut m = 0u32;
    loop {
        // A lone "C-"/"M-"/"S-" is not a prefix: "-" alone is the minus key,
        // and tmux reads "M--" as Meta + minus.
        let (prefix, bit) = match rest.get(..2) {
            Some("C-" | "c-") => (2, mods::CTRL),
            Some("M-" | "m-") => (2, mods::ALT),
            Some("S-" | "s-") => (2, mods::SHIFT),
            _ => break,
        };
        if rest.len() == prefix {
            return None;
        }
        m |= bit;
        rest = &rest[prefix..];
    }
    // tmux's caret notation: ^c is C-c (but a lone "^" is the caret key).
    if let Some(after) = rest.strip_prefix('^') {
        if after.chars().count() == 1 {
            m |= mods::CTRL;
            rest = after;
        }
    }
    // BTab is tmux's Shift+Tab.
    if rest.eq_ignore_ascii_case("BTab") {
        return Some(KeyPress {
            key: "tab",
            mods: m | mods::SHIFT,
            text: None,
        });
    }
    if let Some((_, key, text)) = NAMED.iter().find(|(names, _, _)| names.iter().any(|n| n.eq_ignore_ascii_case(rest))) {
        return Some(KeyPress {
            key,
            mods: m,
            text: text.map(str::to_string),
        });
    }
    let mut chars = rest.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_control() {
        return None;
    }
    if c.is_ascii_uppercase() {
        // "Y" is shift+y typing "Y"; "C-Y" is the same key as "C-y" (tmux
        // folds case under Ctrl).
        if m & mods::CTRL == 0 {
            m |= mods::SHIFT;
        }
    }
    let text = if m & mods::CTRL != 0 { c.to_ascii_lowercase() } else { c };
    Some(KeyPress {
        key: physical(c).unwrap_or(""),
        mods: m,
        text: Some(text.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)] // compared against parse()'s Option
    fn kp(key: &'static str, mods: u32, text: Option<&str>) -> Option<KeyPress> {
        Some(KeyPress {
            key,
            mods,
            text: text.map(str::to_string),
        })
    }

    #[test]
    fn named_keys_and_aliases() {
        assert_eq!(parse("Enter"), kp("enter", 0, None));
        assert_eq!(parse("enter"), kp("enter", 0, None), "named keys are case-insensitive, as in tmux");
        assert_eq!(parse("Esc"), kp("escape", 0, None));
        assert_eq!(parse("PPage"), kp("page_up", 0, None));
        assert_eq!(parse("NPage"), kp("page_down", 0, None));
        assert_eq!(parse("DC"), kp("delete", 0, None));
        assert_eq!(parse("Space"), kp("space", 0, Some(" ")));
        assert_eq!(parse("F12"), kp("f12", 0, None));
        assert_eq!(parse("BTab"), kp("tab", mods::SHIFT, None));
    }

    #[test]
    fn modifier_prefixes() {
        assert_eq!(parse("C-c"), kp("c", mods::CTRL, Some("c")));
        assert_eq!(parse("C-C"), kp("c", mods::CTRL, Some("c")), "tmux folds case under Ctrl");
        assert_eq!(parse("^c"), kp("c", mods::CTRL, Some("c")));
        assert_eq!(parse("M-x"), kp("x", mods::ALT, Some("x")));
        assert_eq!(parse("C-M-x"), kp("x", mods::CTRL | mods::ALT, Some("x")));
        assert_eq!(parse("S-Up"), kp("up", mods::SHIFT, None));
        assert_eq!(parse("M--"), kp("-", mods::ALT, Some("-")));
    }

    #[test]
    fn single_characters() {
        assert_eq!(parse("y"), kp("y", 0, Some("y")));
        assert_eq!(parse("Y"), kp("y", mods::SHIFT, Some("Y")));
        assert_eq!(parse("1"), kp("1", 0, Some("1")));
        assert_eq!(parse("-"), kp("-", 0, Some("-")));
        assert_eq!(parse("^"), kp("", 0, Some("^")), "a lone caret is the caret");
        assert_eq!(parse("é"), kp("", 0, Some("é")), "non-ASCII types its text");
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "C-", "M-", "C-M-", "Bogus", "Enterr", "ab", "\x1b", "\n", "C-\u{7f}"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
