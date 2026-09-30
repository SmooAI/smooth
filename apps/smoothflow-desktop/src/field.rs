//! A one-line text field's state: the text and a caret, edited by keystrokes.
//! Toolkit-free so it's unit-tested; the sheet view only draws it.

/// What a keystroke did to a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// The text changed.
    Changed,
    /// Only the caret moved.
    Moved,
    /// Not a field key: the caller may use it (Enter, Esc, ↑/↓, Tab).
    Ignored,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Field {
    text: String,
    /// Caret as a char index.
    caret: usize,
}

impl Field {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub const fn caret(&self) -> usize {
        self.caret
    }

    /// Replace the text; the caret goes to the end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.caret = self.text.chars().count();
    }

    fn byte(&self, char_index: usize) -> usize {
        self.text.char_indices().nth(char_index).map_or(self.text.len(), |(b, _)| b)
    }

    /// Insert typed text at the caret (newlines dropped: one line).
    pub fn insert(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        if clean.is_empty() {
            return;
        }
        let at = self.byte(self.caret);
        self.text.insert_str(at, &clean);
        self.caret += clean.chars().count();
    }

    /// Apply a key (GPUI's key name, the text it types, and ctrl/alt/platform
    /// held). Ctrl/Super chords other than the line-editing ones are ignored.
    pub fn key(&mut self, key: &str, key_char: Option<&str>, command: bool) -> Edit {
        let len = self.text.chars().count();
        match key {
            "backspace" if self.caret > 0 => {
                let (a, b) = (self.byte(self.caret - 1), self.byte(self.caret));
                self.text.replace_range(a..b, "");
                self.caret -= 1;
                Edit::Changed
            }
            "delete" if self.caret < len => {
                let (a, b) = (self.byte(self.caret), self.byte(self.caret + 1));
                self.text.replace_range(a..b, "");
                Edit::Changed
            }
            "backspace" | "delete" => Edit::Moved,
            "left" => {
                self.caret = self.caret.saturating_sub(1);
                Edit::Moved
            }
            "right" => {
                self.caret = (self.caret + 1).min(len);
                Edit::Moved
            }
            "home" => {
                self.caret = 0;
                Edit::Moved
            }
            "end" => {
                self.caret = len;
                Edit::Moved
            }
            "u" if command => {
                let at = self.byte(self.caret);
                self.text.replace_range(..at, "");
                self.caret = 0;
                Edit::Changed
            }
            _ if command => Edit::Ignored,
            "enter" | "escape" | "up" | "down" | "tab" => Edit::Ignored,
            "space" => {
                self.insert(" ");
                Edit::Changed
            }
            _ => match key_char.filter(|c| !c.is_empty()) {
                Some(c) => {
                    self.insert(c);
                    Edit::Changed
                }
                None => Edit::Ignored,
            },
        }
    }

    /// The text split at the caret, for drawing it.
    #[must_use]
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.byte(self.caret))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_moves_and_deletes_by_character() {
        let mut f = Field::default();
        assert_eq!(f.key("a", Some("a"), false), Edit::Changed);
        f.key("é", Some("é"), false);
        f.key("space", None, false);
        f.key("b", Some("b"), false);
        assert_eq!(f.text(), "aé b");
        assert_eq!(f.key("left", None, false), Edit::Moved);
        f.key("left", None, false);
        f.key("backspace", None, false);
        assert_eq!((f.text(), f.caret()), ("a b", 1));
        f.key("delete", None, false);
        assert_eq!(f.split(), ("a", "b"));
        f.key("home", None, false);
        assert_eq!(f.key("backspace", None, false), Edit::Moved, "nothing before the caret");
        f.key("end", None, false);
        f.key("u", Some("u"), true);
        assert_eq!(f.text(), "", "ctrl-u clears to the caret");
        assert_eq!(f.key("enter", None, false), Edit::Ignored);
        assert_eq!(f.key("n", Some("n"), true), Edit::Ignored, "a chord is not typing");
        f.insert("x\ny");
        assert_eq!(f.text(), "xy");
        f.set("~/dev");
        assert_eq!(f.caret(), 5);
    }
}
