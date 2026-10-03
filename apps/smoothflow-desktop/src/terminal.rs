//! A session's terminal state: PTY bytes in, a grid of styled cells out.
//!
//! Backed by libghostty-vt (th-872ea8), the VT engine every SmoothFlow client
//! uses: the Mac and iOS apps through GhosttyKit, Android and this app through
//! libghostty-vt's C API (`crate::ghostty`). The app only talks to
//! [`TerminalModel`], so the renderer and `Core` never see the backend.

use std::cell::RefCell;

use crate::ghostty::{self, flag, mode, Scroll, Scrollbar, Snapshot, Vt};

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

/// Lines of history libghostty-vt keeps.
const SCROLLBACK: usize = 10_000;

/// The most wheel lines one event turns into reports or arrow keys: a fling
/// on a trackpad must not type hundreds of arrows into vim.
const MAX_WHEEL_LINES: usize = 30;

/// Where a wheel goes, by Ghostty's rules: a program that asked for mouse
/// reports gets them; on the alternate screen with alternate scroll (1007,
/// on by default) it gets arrow keys; otherwise the viewport scrolls the
/// history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelRoute {
    Report,
    ArrowKeys { application: bool },
    Scrollback,
}

impl WheelRoute {
    #[must_use]
    pub const fn of(modes: u32) -> Self {
        if modes & mode::MOUSE != 0 {
            Self::Report
        } else if modes & mode::ALT_SCREEN != 0 && modes & mode::ALT_SCROLL != 0 {
            Self::ArrowKeys {
                application: modes & mode::CURSOR_KEYS != 0,
            }
        } else {
            Self::Scrollback
        }
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
    /// Drawn at reduced alpha (SGR 2).
    pub faint: bool,
    /// Struck through (SGR 9).
    pub strike: bool,
    /// Grid cells the run covers. A wide char counts two: its text holds the
    /// char once, and the spacer cell after it has no text.
    pub cells: usize,
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
    /// A snapshot updates libghostty-vt's render state, so it needs `&mut`;
    /// drawing reads through `&self`.
    vt: RefCell<Vt>,
    cols: usize,
    rows: usize,
}

impl TerminalModel {
    /// # Panics
    /// When libghostty-vt cannot allocate a terminal (out of memory).
    #[must_use]
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = clamp(cols, rows);
        let palette = palette();
        let theme = ghostty::Theme {
            foreground: theme::FOREGROUND,
            background: theme::BACKGROUND,
            cursor: theme::CURSOR,
            palette: &palette,
        };
        let vt = Vt::new(dim(cols), dim(rows), SCROLLBACK, &theme).expect("libghostty-vt could not allocate a terminal");
        Self {
            vt: RefCell::new(vt),
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
        self.vt.get_mut().write(bytes);
    }

    /// Resize the grid; a no-op when unchanged.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = clamp(cols, rows);
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        if self.vt.get_mut().resize(dim(cols), dim(rows)) {
            self.cols = cols;
            self.rows = rows;
        }
    }

    /// The bytes the terminal answered with since the last call (DA and DSR
    /// replies, size reports). They go back to the session as `flow.input`.
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.vt.get_mut().take_replies()
    }

    /// Move the viewport over the history (Shift+PageUp and friends).
    pub fn scroll(&mut self, to: Scroll) {
        self.vt.get_mut().scroll(to);
    }

    /// Where the viewport sits; [`Scrollbar::below`] is how far back it is.
    #[must_use]
    pub fn scrollbar(&self) -> Scrollbar {
        self.vt.borrow_mut().scrollbar()
    }

    /// Whether the viewport shows the live screen (not scrolled back).
    #[must_use]
    pub fn at_bottom(&self) -> bool {
        self.scrollbar().below() == 0
    }

    /// Whether the alternate screen is up (full-screen programs; it has no
    /// history, so Shift+PageUp belongs to the program there).
    #[must_use]
    pub fn alt_screen(&self) -> bool {
        self.vt.borrow_mut().input_modes() & mode::ALT_SCREEN != 0
    }

    /// A wheel of `lines` (up into history is positive) over cell (`col`,
    /// `row`), routed by [`WheelRoute`]. Returns the bytes for the program
    /// (mouse reports or arrow keys); empty when it scrolled the history.
    pub fn wheel(&mut self, lines: isize, col: usize, row: usize) -> Vec<u8> {
        if lines == 0 {
            return Vec::new();
        }
        let vt = self.vt.get_mut();
        let up = lines > 0;
        let n = lines.unsigned_abs().min(MAX_WHEEL_LINES);
        match WheelRoute::of(vt.input_modes()) {
            WheelRoute::Report => {
                let one = vt.mouse_wheel(up, dim(col), dim(row));
                one.repeat(n)
            }
            WheelRoute::ArrowKeys { application } => {
                let key: &[u8] = match (application, up) {
                    (true, true) => b"\x1bOA",
                    (true, false) => b"\x1bOB",
                    (false, true) => b"\x1b[A",
                    (false, false) => b"\x1b[B",
                };
                key.repeat(n)
            }
            WheelRoute::Scrollback => {
                vt.scroll(Scroll::Delta(-lines));
                Vec::new()
            }
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.vt.borrow_mut().snapshot()
    }

    /// Row `line`'s characters, trailing blanks trimmed (tests and snapshots).
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn line_text(&self, line: usize) -> String {
        let s = self.snapshot();
        let mut text = String::new();
        for col in 0..s.cols {
            let Some(cell) = s.cell(line, col) else { break };
            if cell.flags & flag::SPACER != 0 {
                continue;
            }
            text.push(cell_char(cell.codepoint));
            text.extend(&s.extras[line * s.cols + col]);
        }
        text.trim_end().to_string()
    }

    /// The visible screen as styled runs. With `block_cursor`, the cursor
    /// cell is drawn inverted (cursor colour behind, background colour in
    /// front) — the focused pane's block cursor. An unfocused pane draws a
    /// hollow box over [`Screen::cursor`] instead.
    #[must_use]
    pub fn screen(&self, block_cursor: bool) -> Screen {
        let s = self.snapshot();
        let cursor = s.cursor;
        let mut rows = Vec::with_capacity(s.rows);
        for line in 0..s.rows {
            let mut runs: Vec<Run> = Vec::new();
            for col in 0..s.cols {
                let i = line * s.cols + col;
                let cell = &s.cells[i];
                if cell.flags & flag::SPACER != 0 {
                    // The second half of a wide char: no text, one more cell.
                    if let Some(r) = runs.last_mut() {
                        r.cells += 1;
                    }
                    continue;
                }
                let (mut fg, mut bg) = (cell.fg.unwrap_or(theme::FOREGROUND), cell.bg.unwrap_or(theme::BACKGROUND));
                if cell.flags & flag::INVERSE != 0 {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let at_cursor = block_cursor && cursor == Some((line, col));
                if at_cursor {
                    (fg, bg) = (theme::BACKGROUND, theme::CURSOR);
                }
                let bg = (bg != theme::BACKGROUND || at_cursor).then_some(bg);
                let bold = cell.flags & flag::BOLD != 0;
                let italic = cell.flags & flag::ITALIC != 0;
                let underline = cell.flags & flag::UNDERLINE != 0;
                let faint = cell.flags & flag::FAINT != 0;
                let strike = cell.flags & flag::STRIKE != 0;
                let invisible = cell.flags & flag::INVISIBLE != 0;
                let ch = if invisible { ' ' } else { cell_char(cell.codepoint) };
                let extras: &[char] = if invisible { &[] } else { &s.extras[i] };
                match runs.last_mut() {
                    Some(r)
                        if r.fg == fg
                            && r.bg == bg
                            && r.bold == bold
                            && r.italic == italic
                            && r.underline == underline
                            && r.faint == faint
                            && r.strike == strike =>
                    {
                        r.text.push(ch);
                        r.text.extend(extras);
                        r.cells += 1;
                    }
                    _ => {
                        let mut text = ch.to_string();
                        text.extend(extras);
                        runs.push(Run {
                            text,
                            fg,
                            bg,
                            bold,
                            italic,
                            underline,
                            faint,
                            strike,
                            cells: 1,
                        });
                    }
                }
            }
            rows.push(runs);
        }
        Screen { rows, cursor }
    }
}

/// Never a zero-size grid, and never wider or taller than libghostty-vt's u16.
fn clamp(cols: usize, rows: usize) -> (usize, usize) {
    let max = usize::from(u16::MAX);
    (cols.clamp(2, max), rows.clamp(1, max))
}

/// A size `clamp` already bounded.
fn dim(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn cell_char(codepoint: u32) -> char {
    if codepoint == 0 {
        ' '
    } else {
        char::from_u32(codepoint).unwrap_or('\u{fffd}')
    }
}

/// The palette libghostty-vt resolves indexed colours through.
fn palette() -> [Rgb; 256] {
    let mut p = [0; 256];
    for (i, c) in (0..=u8::MAX).zip(p.iter_mut()) {
        *c = indexed(i);
    }
    p
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
        let s = t.screen(false);
        assert_eq!(
            (s.rows.len(), s.rows[0].iter().map(|r| r.cells).sum::<usize>()),
            (10, 40),
            "the grid follows the size"
        );
        t.resize(0, 0);
        assert_eq!(t.size(), (2, 1), "never a zero-size grid");
        assert_eq!(indexed(16), 0x000000);
        assert_eq!(indexed(231), 0xffffff);
        assert_eq!(indexed(232), 0x080808);
    }

    #[test]
    fn resize_reflows_and_keeps_the_text() {
        let mut t = TerminalModel::new(10, 3);
        t.feed(b"0123456789abcde");
        assert_eq!((t.line_text(0).as_str(), t.line_text(1).as_str()), ("0123456789", "abcde"));
        t.resize(20, 3);
        assert_eq!(t.line_text(0), "0123456789abcde", "a soft-wrapped line reflows when widened");
        t.resize(5, 3);
        assert_eq!(t.size(), (5, 3));
        assert!(t.screen(false).rows.iter().all(|r| r.iter().map(|run| run.cells).sum::<usize>() == 5));
    }

    #[test]
    fn sgr_colors_resolve_through_the_theme() {
        let mut t = TerminalModel::new(30, 1);
        // ANSI fg, bright fg, 256-colour fg, truecolor fg, ANSI bg, italic +
        // underline, inverse.
        t.feed(b"\x1b[32ma\x1b[94mb\x1b[38;5;196mc\x1b[38;2;1;2;3md\x1b[0;45me\x1b[0;3;4mf\x1b[0;7mg\x1b[0m");
        let runs = t.screen(false).rows.remove(0);
        let by = |s: &str| {
            runs.iter()
                .find(|r| r.text.starts_with(s))
                .cloned()
                .unwrap_or_else(|| panic!("no run {s:?} in {runs:?}"))
        };
        assert_eq!(by("a").fg, theme::ANSI[2]);
        assert_eq!(by("b").fg, theme::ANSI[12]);
        assert_eq!(by("c").fg, indexed(196));
        assert_eq!(by("c").fg, 0xff0000);
        assert_eq!(by("d").fg, 0x010203);
        assert_eq!((by("e").fg, by("e").bg), (theme::FOREGROUND, Some(theme::ANSI[5])));
        assert!(by("f").italic && by("f").underline && !by("f").bold);
        assert_eq!(
            (by("g").fg, by("g").bg),
            (theme::BACKGROUND, Some(theme::FOREGROUND)),
            "inverse swaps the defaults"
        );
        assert_eq!(runs.last().map(|r| r.bg), Some(None), "the rest of the line is on the pane background");
    }

    #[test]
    fn wide_chars_take_two_cells() {
        let mut t = TerminalModel::new(10, 1);
        t.feed("a漢b".as_bytes());
        assert_eq!(t.line_text(0), "a漢b");
        let s = t.screen(false);
        assert_eq!(s.cursor, Some((0, 4)), "a(1) + 漢(2) + b(1)");
        let row = &s.rows[0];
        assert_eq!(row.iter().map(|r| r.cells).sum::<usize>(), 10, "every cell accounted for");
        assert!(row[0].text.starts_with("a漢b"));
        // The cursor on a wide char's second half sits on the char.
        t.feed(b"\r\x1b[2C");
        assert_eq!(t.screen(false).cursor, Some((0, 1)));
    }

    #[test]
    fn device_attribute_and_status_queries_are_answered() {
        let mut t = TerminalModel::new(20, 5);
        assert!(t.take_replies().is_empty());
        t.feed(b"\x1b[c");
        assert_eq!(t.take_replies(), b"\x1b[?62;22c", "DA1: a VT220 with ANSI colour, as Ghostty answers");
        t.feed(b"\x1b[3;7H\x1b[6n");
        assert_eq!(t.take_replies(), b"\x1b[3;7R", "DSR cursor position, 1-based");
        t.feed(b"\x1b[5n");
        assert_eq!(t.take_replies(), b"\x1b[0n", "DSR status: OK");
        assert!(t.take_replies().is_empty(), "replies are taken once");
    }

    #[test]
    fn the_alternate_screen_comes_and_goes() {
        let mut t = TerminalModel::new(20, 3);
        t.feed(b"shell$ ");
        t.feed(b"\x1b[?1049h\x1b[H\x1b[2Jfull-screen app");
        assert_eq!(t.line_text(0), "full-screen app");
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.line_text(0), "shell$", "leaving restores the primary screen");
        assert_eq!(t.screen(false).cursor, Some((0, 7)), "and its cursor");
    }

    #[test]
    fn faint_and_strikethrough_are_their_own_runs() {
        let mut t = TerminalModel::new(20, 1);
        t.feed(b"a\x1b[2mb\x1b[22;9mc\x1b[2md\x1b[0me");
        let runs = t.screen(false).rows.remove(0);
        let by = |s: &str| {
            runs.iter()
                .find(|r| r.text.starts_with(s))
                .cloned()
                .unwrap_or_else(|| panic!("no run {s:?} in {runs:?}"))
        };
        assert!(!by("a").faint && !by("a").strike);
        assert!(by("b").faint && !by("b").strike, "SGR 2");
        assert!(!by("c").faint && by("c").strike, "SGR 22 ends faint, 9 strikes");
        assert!(by("d").faint && by("d").strike, "both");
        assert!(!by("e").faint && !by("e").strike, "SGR 0 resets both");
    }

    /// `n` numbered lines, so a viewport's first row says where it is.
    fn numbered(t: &mut TerminalModel, n: usize) {
        for i in 1..=n {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
    }

    #[test]
    fn the_viewport_scrolls_the_history_and_new_output_leaves_it_be() {
        let mut t = TerminalModel::new(20, 5);
        numbered(&mut t, 50);
        assert!(t.at_bottom());
        assert_eq!(t.line_text(0), "line 47", "the screen: lines 47–50 and the prompt row");
        let sb = t.scrollbar();
        assert_eq!((sb.total, sb.len, sb.below()), (51, 5, 0));

        assert!(t.wheel(3, 0, 0).is_empty(), "a plain shell: the wheel scrolls, nothing is sent");
        assert_eq!(t.line_text(0), "line 44");
        assert_eq!(t.scrollbar().below(), 3);
        assert!(!t.at_bottom());
        assert_eq!(t.screen(true).cursor, None, "the cursor is below the viewport");

        t.feed(b"line 51\r\n");
        assert_eq!(t.line_text(0), "line 44", "new output does not pull the viewport down (Ghostty's default)");
        assert_eq!(t.scrollbar().below(), 4);

        t.scroll(Scroll::Top);
        assert_eq!(t.line_text(0), "line 1");
        t.scroll(Scroll::Delta(2));
        assert_eq!(t.line_text(0), "line 3");
        assert!(t.wheel(-1, 0, 0).is_empty());
        assert_eq!(t.line_text(0), "line 4", "wheel down moves toward the screen");
        t.scroll(Scroll::Bottom);
        assert!(t.at_bottom());
        assert_eq!(t.line_text(0), "line 48");
        assert!(t.wheel(-5, 0, 0).is_empty());
        assert!(t.at_bottom(), "never below the screen");
    }

    #[test]
    fn the_alternate_screen_turns_the_wheel_into_arrow_keys() {
        let mut t = TerminalModel::new(20, 5);
        numbered(&mut t, 30);
        t.feed(b"\x1b[?1049h");
        assert!(t.alt_screen());
        assert_eq!(t.wheel(2, 0, 0), b"\x1b[A\x1b[A", "alternate scroll (1007) is on by default");
        assert_eq!(t.wheel(-1, 0, 0), b"\x1b[B");
        t.feed(b"\x1b[?1h");
        assert_eq!(t.wheel(1, 0, 0), b"\x1bOA", "application cursor keys");
        assert!(t.at_bottom(), "no history scrolled meanwhile");
        t.feed(b"\x1b[?1007l");
        assert!(t.wheel(1, 0, 0).is_empty(), "1007 off: nothing to scroll and nothing sent");
        t.feed(b"\x1b[?1049l");
        assert!(!t.alt_screen());
        assert_eq!(t.wheel(40, 0, 0), b"", "back on the primary screen it scrolls again");
        assert!(!t.at_bottom());
    }

    #[test]
    fn a_program_tracking_the_mouse_gets_wheel_reports() {
        let mut t = TerminalModel::new(20, 5);
        numbered(&mut t, 30);
        // Normal tracking, SGR format — what vim, htop and Claude Code ask for.
        t.feed(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(t.wheel(1, 3, 2), b"\x1b[<64;4;3M", "button 4 at column 4, row 3 (1-based)");
        assert_eq!(t.wheel(-2, 0, 0), b"\x1b[<65;1;1M\x1b[<65;1;1M", "button 5, once per line");
        assert!(t.at_bottom(), "the history never moved");
        assert_eq!(t.wheel(500, 0, 0).len(), MAX_WHEEL_LINES * b"\x1b[<64;1;1M".len(), "a fling is capped");
        t.feed(b"\x1b[?1000l");
        assert!(t.wheel(1, 0, 0).is_empty(), "tracking off: the history scrolls again");
        assert!(!t.at_bottom());
    }

    #[test]
    fn wheel_routing_follows_ghostty() {
        assert_eq!(WheelRoute::of(0), WheelRoute::Scrollback);
        assert_eq!(
            WheelRoute::of(mode::ALT_SCROLL),
            WheelRoute::Scrollback,
            "1007 only matters on the alternate screen"
        );
        assert_eq!(WheelRoute::of(mode::ALT_SCREEN), WheelRoute::Scrollback);
        assert_eq!(
            WheelRoute::of(mode::ALT_SCREEN | mode::ALT_SCROLL | mode::CURSOR_KEYS),
            WheelRoute::ArrowKeys { application: true }
        );
        assert_eq!(
            WheelRoute::of(mode::MOUSE | mode::ALT_SCREEN | mode::ALT_SCROLL),
            WheelRoute::Report,
            "reports win"
        );
    }

    #[test]
    fn combining_marks_stay_with_their_cell() {
        let mut t = TerminalModel::new(10, 1);
        t.feed("e\u{301}x".as_bytes());
        assert_eq!(t.line_text(0), "e\u{301}x");
        assert_eq!(t.screen(false).cursor, Some((0, 2)));
    }
}
