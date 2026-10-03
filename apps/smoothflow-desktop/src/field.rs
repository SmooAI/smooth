//! A text field's state: the text and a caret, edited by keystrokes. One line
//! by default; `Field::multiline` keeps newlines (the Diff tab's comment).
//! Toolkit-free so it's unit-tested; the views only draw it.

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
    /// Newlines are text, and ↑/↓/Home/End work by line.
    multi: bool,
}

impl Field {
    /// An empty field that keeps newlines.
    #[must_use]
    pub fn multiline() -> Self {
        Self {
            multi: true,
            ..Self::default()
        }
    }

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

    /// Insert typed text at the caret. Control characters are dropped, and
    /// newlines too unless the field is multi-line (`\r\n` and `\r` count as
    /// one newline).
    pub fn insert(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let clean: String = s.chars().filter(|&c| !c.is_control() || (self.multi && c == '\n')).collect();
        if clean.is_empty() {
            return;
        }
        let at = self.byte(self.caret);
        self.text.insert_str(at, &clean);
        self.caret += clean.chars().count();
    }

    /// The caret's line and column (both char-based, from 0).
    #[must_use]
    pub fn caret_line_col(&self) -> (usize, usize) {
        let before: Vec<char> = self.text.chars().take(self.caret).collect();
        let line = before.iter().filter(|&&c| c == '\n').count();
        let col = before.iter().rev().take_while(|&&c| c != '\n').count();
        (line, col)
    }

    /// The char index at `line`/`col`, the column clamped to the line.
    fn index_at(&self, line: usize, col: usize) -> usize {
        let mut start = 0;
        for (i, l) in self.text.split('\n').enumerate() {
            let n = l.chars().count();
            if i == line {
                return start + col.min(n);
            }
            start += n + 1;
        }
        self.text.chars().count()
    }

    /// Move the caret one line up or down, keeping its column where the line
    /// allows. `false` at the first/last line.
    fn move_line(&mut self, down: bool) -> bool {
        let (line, col) = self.caret_line_col();
        let lines = self.text.split('\n').count();
        if (down && line + 1 >= lines) || (!down && line == 0) {
            return false;
        }
        self.caret = self.index_at(if down { line + 1 } else { line - 1 }, col);
        true
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
                let (line, _) = self.caret_line_col();
                self.caret = if self.multi { self.index_at(line, 0) } else { 0 };
                Edit::Moved
            }
            "end" => {
                let (line, _) = self.caret_line_col();
                self.caret = if self.multi { self.index_at(line, usize::MAX) } else { len };
                Edit::Moved
            }
            "up" | "down" if self.multi => {
                self.move_line(key == "down");
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

    /// The text's lines (a multi-line field's, for drawing).
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.text.split('\n')
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
        assert_eq!(f.key("up", None, false), Edit::Ignored, "one line: ↑ is the caller's");
    }

    #[test]
    fn a_multiline_field_keeps_newlines_and_moves_by_line() {
        let mut f = Field::multiline();
        f.insert("first line\r\nab\rlast\tx");
        assert_eq!(f.text(), "first line\nab\nlastx", "CRLF and CR become one newline; other controls drop");
        assert_eq!(f.lines().collect::<Vec<_>>(), ["first line", "ab", "lastx"]);
        assert_eq!(f.caret_line_col(), (2, 5));
        assert_eq!(f.key("up", None, false), Edit::Moved);
        assert_eq!(f.caret_line_col(), (1, 2), "the column clamps to the shorter line");
        f.key("up", None, false);
        assert_eq!(f.caret_line_col(), (0, 2), "back up, at the clamped column");
        f.key("up", None, false);
        assert_eq!(f.caret_line_col(), (0, 2), "the first line stays put");
        f.key("end", None, false);
        assert_eq!(f.caret_line_col(), (0, 10), "End is the line's end");
        f.key("down", None, false);
        f.key("home", None, false);
        assert_eq!(f.caret_line_col(), (1, 0), "Home is the line's start");
        f.key("backspace", None, false);
        assert_eq!(f.text(), "first lineab\nlastx", "backspace joins lines");
        f.key("end", None, false);
        f.key("down", None, false);
        f.key("down", None, false);
        assert_eq!(f.caret_line_col(), (1, 5), "the last line stays put");
        assert_eq!(f.key("enter", None, false), Edit::Ignored, "Enter is the caller's (newline or submit)");
    }
}
