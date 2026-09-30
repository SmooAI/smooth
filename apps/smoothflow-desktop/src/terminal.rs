//! A session's terminal state: PTY bytes in, a grid of styled cells out.
//!
//! Backed by `alacritty_terminal` (the VT parser and grid Zed also uses). The
//! app only talks to [`TerminalModel`], so libghostty-vt can replace the
//! backend without touching the renderer.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

/// An sRGB color, `0xRRGGBB`.
pub type Rgb = u32;

/// Catppuccin Mocha: the SmoothFlow default theme (Client Spec §10).
pub mod theme {
    use super::Rgb;
    pub const FOREGROUND: Rgb = 0xcdd6f4;
    pub const BACKGROUND: Rgb = 0x1e1e2e;
    pub const CURSOR: Rgb = 0xf5e0dc;
    /// ANSI 0–15.
    pub const ANSI: [Rgb; 16] = [
        0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5,
        0xa6adc8,
    ];
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// One run of same-styled cells in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// The visible screen, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub rows: Vec<Vec<Run>>,
    /// Cursor `(row, col)` when the program shows it.
    pub cursor: Option<(usize, usize)>,
}

/// A session's terminal.
pub struct TerminalModel {
    term: Term<VoidListener>,
    parser: Processor,
    cols: usize,
    rows: usize,
}

impl TerminalModel {
    #[must_use]
    pub fn new(cols: usize, rows: usize) -> Self {
        let size = Size { cols, rows };
        Self {
            term: Term::new(Config::default(), &size, VoidListener),
            parser: Processor::new(),
            cols,
            rows,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    /// Feed PTY output.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the grid; a no-op when unchanged.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(Size { cols, rows });
    }

    /// Row `line`'s characters, trailing blanks trimmed (tests and snapshots).
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn line_text(&self, line: usize) -> String {
        let row = &self.term.grid()[Line(i32::try_from(line).unwrap_or(0))];
        (0..self.cols).map(|c| row[Column(c)].c).collect::<String>().trim_end().to_string()
    }

    /// The visible screen as styled runs. With `block_cursor`, the cursor
    /// cell is drawn inverted (cursor colour behind, background colour in
    /// front) — the focused pane's block cursor. An unfocused pane draws a
    /// hollow box over [`Screen::cursor`] instead.
    #[must_use]
    pub fn screen(&self, block_cursor: bool) -> Screen {
        let grid = self.term.grid();
        let point = grid.cursor.point;
        let cursor = if self.term.mode().contains(TermMode::SHOW_CURSOR) {
            usize::try_from(point.line.0)
                .ok()
                .filter(|l| *l < self.rows)
                .map(|l| (l, point.column.0.min(self.cols - 1)))
        } else {
            None
        };
        let mut rows = Vec::with_capacity(self.rows);
        for line in 0..self.rows {
            let row = &grid[Line(i32::try_from(line).unwrap_or(0))];
            let mut runs: Vec<Run> = Vec::new();
            for col in 0..self.cols {
                let cell = &row[Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let inverse = cell.flags.contains(Flags::INVERSE);
                let (mut fg, mut bg) = (resolve(cell.fg, true), resolve(cell.bg, false));
                if inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if block_cursor && cursor == Some((line, col)) {
                    (fg, bg) = (theme::BACKGROUND, theme::CURSOR);
                }
                let bg = (bg != theme::BACKGROUND || (block_cursor && cursor == Some((line, col)))).then_some(bg);
                let bold = cell.flags.contains(Flags::BOLD);
                let italic = cell.flags.contains(Flags::ITALIC);
                let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                match runs.last_mut() {
                    Some(r) if r.fg == fg && r.bg == bg && r.bold == bold && r.italic == italic && r.underline == underline => r.text.push(ch),
                    _ => runs.push(Run {
                        text: ch.to_string(),
                        fg,
                        bg,
                        bold,
                        italic,
                        underline,
                    }),
                }
            }
            rows.push(runs);
        }
        Screen { rows, cursor }
    }
}

/// A cell color on the theme.
fn resolve(c: Color, foreground: bool) -> Rgb {
    match c {
        Color::Spec(rgb) => (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b),
        Color::Indexed(i) => indexed(i),
        Color::Named(n) => match n {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => theme::FOREGROUND,
            NamedColor::Background => theme::BACKGROUND,
            NamedColor::Cursor => theme::CURSOR,
            other => {
                let i = other as usize;
                if i < 16 {
                    theme::ANSI[i]
                } else if foreground {
                    theme::FOREGROUND
                } else {
                    theme::BACKGROUND
                }
            }
        },
    }
}

/// xterm's 256-color palette: 16 theme colors, a 6×6×6 cube, 24 greys.
fn indexed(i: u8) -> Rgb {
    match i {
        0..=15 => theme::ANSI[usize::from(i)],
        16..=231 => {
            let n = u32::from(i - 16);
            let level = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
            (level(n / 36) << 16) | (level((n / 6) % 6) << 8) | level(n % 6)
        }
        _ => {
            let g = 8 + u32::from(i - 232) * 10;
            (g << 16) | (g << 8) | g
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_become_a_styled_grid() {
        let mut t = TerminalModel::new(20, 3);
        t.feed(b"hello\r\n\x1b[1;31mred\x1b[0m done");
        assert_eq!(t.line_text(0), "hello");
        assert_eq!(t.line_text(1), "red done");
        let s = t.screen(false);
        let red = &s.rows[1][0];
        assert_eq!((red.text.as_str(), red.fg, red.bold), ("red", theme::ANSI[1], true));
        assert_eq!(s.rows[1][1].text.trim(), "done", "the reset run carries the space before it");
        assert_eq!(s.cursor, Some((1, 8)));
    }

    #[test]
    fn the_block_cursor_inverts_its_cell_and_hides_with_the_program() {
        let mut t = TerminalModel::new(10, 2);
        t.feed(b"ab");
        let s = t.screen(true);
        assert_eq!(s.cursor, Some((0, 2)));
        let cell = s.rows[0].iter().find(|r| r.bg == Some(theme::CURSOR)).map(|r| (r.text.clone(), r.fg));
        assert_eq!(cell, Some((" ".to_string(), theme::BACKGROUND)), "the cell under the cursor, inverted");
        assert_eq!(s.rows[0][0].text, "ab");
        assert!(t.screen(false).rows[0].iter().all(|r| r.bg.is_none()), "no block when unfocused");
        t.feed(b"\x1b[?25l");
        assert_eq!(t.screen(true).cursor, None, "DECTCEM off hides it");
    }

    #[test]
    fn resize_and_palette() {
        let mut t = TerminalModel::new(10, 2);
        t.resize(40, 10);
        assert_eq!(t.size(), (40, 10));
        t.resize(0, 0);
        assert_eq!(t.size(), (2, 1), "never a zero-size grid");
        assert_eq!(indexed(16), 0x000000);
        assert_eq!(indexed(231), 0xffffff);
        assert_eq!(indexed(232), 0x080808);
    }
}
