//! Keystrokes to the bytes a terminal program expects (xterm conventions).

/// A keystroke, toolkit-free: GPUI's `Keystroke` maps onto this one to one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Key<'a> {
    /// The key's name (`a`, `enter`, `left`, …).
    pub key: &'a str,
    /// What it types, when it types something.
    pub key_char: Option<&'a str>,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// ⌘ / Super. Never sent: those are the app's shortcuts.
    pub platform: bool,
}

/// The chord a keystroke is, for the keymap (`smooth-flow-client::keymap`).
#[must_use]
pub fn chord(k: Key<'_>) -> smooth_flow_client::keymap::Chord {
    smooth_flow_client::keymap::Chord {
        key: k.key.to_lowercase(),
        ctrl: k.control,
        alt: k.alt,
        shift: k.shift,
        cmd: k.platform,
    }
}

/// The bytes for a keystroke, or `None` when the terminal gets nothing (an
/// app shortcut, or a lone modifier).
#[must_use]
pub fn encode(k: Key<'_>) -> Option<Vec<u8>> {
    if k.platform {
        return None;
    }
    let named: Option<&[u8]> = match k.key {
        "enter" => Some(b"\r"),
        "backspace" => Some(b"\x7f"),
        "tab" if k.shift => Some(b"\x1b[Z"),
        "tab" => Some(b"\t"),
        "escape" => Some(b"\x1b"),
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "right" => Some(b"\x1b[C"),
        "left" => Some(b"\x1b[D"),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        "delete" => Some(b"\x1b[3~"),
        _ => None,
    };
    if let Some(bytes) = named {
        let mut v = Vec::with_capacity(bytes.len() + 1);
        if k.alt {
            v.push(0x1b);
        }
        v.extend_from_slice(bytes);
        return Some(v);
    }
    if k.control {
        let c = k.key.chars().next().filter(|_| k.key.chars().count() == 1)?.to_ascii_lowercase();
        let byte = match c {
            'a'..='z' => (c as u8) - b'a' + 1,
            '@' | ' ' | '2' => 0,
            '[' | '3' => 0x1b,
            '\\' | '4' => 0x1c,
            ']' | '5' => 0x1d,
            '6' => 0x1e,
            '/' | '7' | '-' => 0x1f,
            _ => return None,
        };
        return Some(if k.alt { vec![0x1b, byte] } else { vec![byte] });
    }
    let text = k.key_char.or_else(|| (k.key == "space").then_some(" "))?;
    if text.is_empty() {
        return None;
    }
    let mut v = Vec::with_capacity(text.len() + 1);
    if k.alt {
        v.push(0x1b);
    }
    v.extend_from_slice(text.as_bytes());
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: &str) -> Key<'_> {
        Key { key, ..Key::default() }
    }

    #[test]
    fn keys_encode_like_xterm() {
        assert_eq!(encode(Key { key_char: Some("a"), ..k("a") }), Some(b"a".to_vec()));
        assert_eq!(
            encode(Key {
                key_char: Some("é"), ..k("e")
            }),
            Some("é".as_bytes().to_vec())
        );
        assert_eq!(encode(k("enter")), Some(b"\r".to_vec()));
        assert_eq!(encode(k("up")), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(Key { shift: true, ..k("tab") }), Some(b"\x1b[Z".to_vec()));
        assert_eq!(encode(Key { control: true, ..k("c") }), Some(vec![3]));
        assert_eq!(encode(Key { control: true, ..k("d") }), Some(vec![4]));
        assert_eq!(encode(Key { control: true, ..k("[") }), Some(vec![0x1b]));
        assert_eq!(
            encode(Key {
                alt: true,
                key_char: Some("b"),
                ..k("b")
            }),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            encode(Key {
                platform: true,
                key_char: Some("c"),
                ..k("c")
            }),
            None,
            "⌘C is the app's"
        );
        assert_eq!(encode(k("shift")), None);
        assert_eq!(encode(k("space")), Some(b" ".to_vec()));
    }

    #[test]
    fn keystrokes_become_keymap_chords() {
        use smooth_flow_client::keymap::{Action, Chord, Keymap, Platform};
        let c = chord(Key {
            control: true,
            shift: true,
            ..k("N")
        });
        assert_eq!(Some(c.clone()), Chord::parse("ctrl+shift+n"));
        assert_eq!(Keymap::defaults(Platform::Other).action_for(&c), Some(Action::NewSession));
        let c = chord(Key {
            control: true,
            alt: true,
            ..k("left")
        });
        assert_eq!(Keymap::defaults(Platform::Other).action_for(&c), Some(Action::FocusPaneLeft));
        assert_eq!(
            Keymap::defaults(Platform::Other).action_for(&chord(Key { control: true, ..k("c") })),
            None,
            "Ctrl+C is the program's"
        );
    }
}
