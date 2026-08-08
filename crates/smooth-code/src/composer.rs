//! Wrap-aware geometry for the message input box.
//!
//! The input used to be one clipped row: text past the box width simply
//! vanished, `\n` was rewritten to a space on both typing and paste, and the
//! cursor was placed by adding the raw **byte** offset to `inner.x` — so any
//! multi-byte character drifted it. This module is the arithmetic that lets the
//! box wrap, grow with its content, and scroll (pearl th-958e2e).
//!
//! It is deliberately pure — no ratatui `Frame`, no `AppState` — so the layout
//! rules are unit-testable, and so the renderer and the height calculation can
//! never disagree about how many rows the same text occupies.
//!
//! Widths are in terminal **columns**, matching `Wrap { trim: false }`: a
//! logical line of `w` columns occupies `ceil(w / width)` rows, and an empty
//! line still occupies one.

/// Rows the input box will never grow beyond, before it starts scrolling.
///
/// The inline viewport is only ~14 rows tall and is fixed at startup
/// (`Viewport::Inline` in `app.rs`), so the box borrows space from the
/// streaming preview above it. Six text rows is enough for a substantial
/// prompt while leaving the preview usable.
pub const MAX_TEXT_ROWS: u16 = 6;

/// The box's growth ceiling for a given inline-viewport height — responsive
/// instead of hard-capped at [`MAX_TEXT_ROWS`] (pearl th-d5eb9f). Reserves 8
/// rows for the streaming preview, status line, and borders; the old constant
/// stays as the floor so short terminals behave exactly as before.
#[must_use]
pub fn max_text_rows(viewport_h: u16) -> u16 {
    viewport_h.saturating_sub(8).clamp(MAX_TEXT_ROWS, 16)
}

/// Display width of a `char` in terminal columns.
///
/// Deliberately narrow: control characters are zero-width, everything else is
/// one column. `th code` has no CJK/emoji-aware width dependency and adding one
/// for the input alone would be a new dependency for a rare case — a wide glyph
/// costs one column of accuracy in cursor placement, which self-corrects on the
/// next keystroke.
///
/// `ponytail:` swap for `unicode-width` if double-width text becomes a real
/// complaint; the call sites all route through here.
fn char_cols(ch: char) -> usize {
    if ch == '\t' {
        4
    } else if ch.is_control() {
        0
    } else {
        1
    }
}

/// Split `text` into the rows it occupies when wrapped at `width` columns.
///
/// Returns byte ranges into `text`, one per rendered row, so callers can slice
/// without reallocating. Always returns at least one row (an empty input still
/// needs a row for the cursor).
#[must_use]
pub fn wrap_rows(text: &str, width: u16) -> Vec<std::ops::Range<usize>> {
    let width = usize::from(width).max(1);
    let mut rows = Vec::new();

    for line in split_logical_lines(text) {
        let mut row_start = line.start;
        let mut cols = 0usize;
        for (offset, ch) in text[line.clone()].char_indices() {
            let at = line.start + offset;
            let c = char_cols(ch);
            // A char that would overflow the row starts the next one. `>=` is
            // wrong here: a row exactly `width` columns wide is full, but the
            // break belongs *before* the char that overflows it, not after the
            // one that fills it.
            if cols + c > width && at > row_start {
                rows.push(row_start..at);
                row_start = at;
                cols = 0;
            }
            cols += c;
        }
        rows.push(row_start..line.end);
    }

    if rows.is_empty() {
        rows.push(0..0);
    }
    rows
}

/// Byte ranges of the `\n`-separated logical lines in `text`, excluding the
/// newline itself. A trailing newline yields a final empty line, which is what
/// makes the cursor land on a fresh row after you press Enter.
fn split_logical_lines(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (idx, _) in text.match_indices('\n') {
        lines.push(start..idx);
        start = idx + 1;
    }
    lines.push(start..text.len());
    lines
}

/// Row and column of the cursor, given a **byte** offset into `text`.
///
/// The cursor sits at the end of the row whose range contains it, which is what
/// puts it after the last character you typed rather than wrapping it early to
/// the next row.
#[must_use]
pub fn cursor_position(text: &str, cursor: usize, width: u16) -> (u16, u16) {
    let cursor = cursor.min(text.len());
    let rows = wrap_rows(text, width);

    for (idx, row) in rows.iter().enumerate() {
        let is_last = idx + 1 == rows.len();
        // `cursor == row.end` belongs to THIS row unless a later row starts
        // there — otherwise a cursor at a wrap point renders a row too high.
        let owns_end = is_last || rows[idx + 1].start > cursor;
        if cursor >= row.start && (cursor < row.end || (cursor == row.end && owns_end)) {
            let col = text[row.start..cursor].chars().map(char_cols).sum::<usize>();
            return (u16::try_from(idx).unwrap_or(u16::MAX), u16::try_from(col).unwrap_or(u16::MAX));
        }
    }

    let last = rows.len().saturating_sub(1);
    (u16::try_from(last).unwrap_or(u16::MAX), 0)
}

/// Rows of text the box wants, clamped to [1, `cap`] (see [`max_text_rows`]).
#[must_use]
pub fn desired_text_rows(text: &str, width: u16, cap: u16) -> u16 {
    let rows = u16::try_from(wrap_rows(text, width).len()).unwrap_or(u16::MAX);
    rows.clamp(1, cap.max(1))
}

/// Clamp a scroll offset to the draft's extent, leaving it where it is.
///
/// Used once the user has scrolled by hand: the view stays put instead of
/// snapping back to the cursor, which is what every editor does — a wheel
/// scroll doesn't move the caret.
#[must_use]
pub fn clamp_to_bounds(current: u16, total_rows: u16, visible_rows: u16) -> u16 {
    current.min(total_rows.saturating_sub(visible_rows.max(1)))
}

/// Scroll offset that keeps the cursor on screen.
///
/// Returns the first visible row. Scrolls the minimum needed: content shorter
/// than the box never scrolls, and the offset is clamped so the last row can't
/// be scrolled past into empty space.
#[must_use]
pub fn clamp_scroll(current: u16, cursor_row: u16, total_rows: u16, visible_rows: u16) -> u16 {
    let visible = visible_rows.max(1);
    let max_scroll = total_rows.saturating_sub(visible);
    let mut scroll = current.min(max_scroll);
    if cursor_row < scroll {
        scroll = cursor_row;
    } else if cursor_row >= scroll + visible {
        scroll = cursor_row.saturating_sub(visible - 1);
    }
    scroll.min(max_scroll)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_as_str(text: &str, width: u16) -> Vec<&str> {
        wrap_rows(text, width).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn empty_input_still_occupies_one_row() {
        assert_eq!(wrap_rows("", 10).len(), 1);
        assert_eq!(desired_text_rows("", 10, MAX_TEXT_ROWS), 1);
        assert_eq!(cursor_position("", 0, 10), (0, 0));
    }

    #[test]
    fn short_text_is_a_single_row() {
        assert_eq!(rows_as_str("hello", 10), vec!["hello"]);
        assert_eq!(desired_text_rows("hello", 10, MAX_TEXT_ROWS), 1);
    }

    /// The old renderer clipped here — everything past the box width was
    /// invisible while remaining in the buffer, so users lost track of what
    /// they had typed.
    #[test]
    fn long_text_soft_wraps_at_the_box_width() {
        assert_eq!(rows_as_str("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(desired_text_rows("abcdefghij", 4, MAX_TEXT_ROWS), 3);
    }

    #[test]
    fn text_exactly_the_box_width_does_not_wrap() {
        assert_eq!(rows_as_str("abcd", 4), vec!["abcd"]);
        assert_eq!(desired_text_rows("abcd", 4, MAX_TEXT_ROWS), 1);
    }

    #[test]
    fn newlines_start_new_rows() {
        assert_eq!(rows_as_str("ab\ncd", 10), vec!["ab", "cd"]);
        assert_eq!(desired_text_rows("ab\ncd", 10, MAX_TEXT_ROWS), 2);
    }

    /// Pressing Enter at the end must give the cursor a row to sit on.
    #[test]
    fn trailing_newline_yields_an_empty_final_row() {
        assert_eq!(rows_as_str("ab\n", 10), vec!["ab", ""]);
        assert_eq!(cursor_position("ab\n", 3, 10), (1, 0));
    }

    #[test]
    fn blank_lines_each_take_a_row() {
        assert_eq!(rows_as_str("a\n\n\nb", 10), vec!["a", "", "", "b"]);
    }

    #[test]
    fn growth_is_capped_then_the_box_scrolls() {
        let tall = "x\n".repeat(usize::from(MAX_TEXT_ROWS) + 5);
        assert!(wrap_rows(&tall, 10).len() > usize::from(MAX_TEXT_ROWS));
        assert_eq!(desired_text_rows(&tall, 10, MAX_TEXT_ROWS), MAX_TEXT_ROWS);
    }

    /// The cursor used to be `inner.x + byte_offset`, so any multi-byte char
    /// pushed it right by the extra bytes. Columns are characters, not bytes.
    #[test]
    fn cursor_columns_are_characters_not_bytes() {
        let text = "héllo"; // `é` is two bytes
        assert_eq!(text.len(), 6, "precondition: byte length exceeds char count");
        assert_eq!(cursor_position(text, text.len(), 10), (0, 5));
    }

    #[test]
    fn cursor_tracks_across_a_soft_wrap() {
        // "abcdefghij" at width 4 -> ["abcd", "efgh", "ij"]
        assert_eq!(cursor_position("abcdefghij", 0, 4), (0, 0));
        assert_eq!(cursor_position("abcdefghij", 4, 4), (1, 0));
        assert_eq!(cursor_position("abcdefghij", 8, 4), (2, 0));
        assert_eq!(cursor_position("abcdefghij", 10, 4), (2, 2));
    }

    #[test]
    fn cursor_at_end_of_a_full_row_stays_on_that_row() {
        // Nothing follows, so the cursor sits just past the last column.
        assert_eq!(cursor_position("abcd", 4, 4), (0, 4));
    }

    #[test]
    fn cursor_beyond_the_text_is_clamped_not_panicking() {
        assert_eq!(cursor_position("ab", 99, 10), (0, 2));
    }

    #[test]
    fn zero_width_does_not_divide_by_zero() {
        assert!(!wrap_rows("abc", 0).is_empty());
        assert_eq!(desired_text_rows("abc", 0, MAX_TEXT_ROWS), 3);
    }

    #[test]
    fn content_shorter_than_the_box_never_scrolls() {
        assert_eq!(clamp_scroll(0, 0, 2, 6), 0);
        assert_eq!(clamp_scroll(4, 1, 3, 6), 0, "a stale offset is clamped back down");
    }

    #[test]
    fn scroll_follows_the_cursor_down_and_back_up() {
        // 20 rows of content in a 6-row box.
        assert_eq!(clamp_scroll(0, 5, 20, 6), 0, "still visible, no scroll");
        assert_eq!(clamp_scroll(0, 6, 20, 6), 1, "one row past the bottom");
        assert_eq!(clamp_scroll(10, 2, 20, 6), 2, "cursor above the window pulls it up");
    }

    #[test]
    fn scroll_cannot_run_past_the_last_row() {
        assert_eq!(clamp_scroll(99, 19, 20, 6), 14);
        assert_eq!(clamp_scroll(99, 0, 20, 6), 0);
    }

    /// After a wheel scroll the view must stay where it was put, even though
    /// the cursor is somewhere else entirely — otherwise every frame snaps
    /// back to the caret and the wheel appears not to work at all.
    #[test]
    fn hand_scrolling_does_not_snap_back_to_the_cursor() {
        assert_eq!(clamp_to_bounds(3, 20, 6), 3);
        assert_eq!(clamp_to_bounds(14, 20, 6), 14, "the last full window");
        assert_eq!(clamp_to_bounds(99, 20, 6), 14, "still clamped to the end");
        assert_eq!(clamp_to_bounds(5, 3, 6), 0, "content that fits never scrolls");
    }

    #[test]
    fn tabs_advance_multiple_columns() {
        assert_eq!(cursor_position("\ta", 2, 20), (0, 5));
    }

    /// Every row range must be a valid slice of the source, at any width, or
    /// the renderer panics with "byte index is not a char boundary".
    #[test]
    fn rows_are_always_valid_slices_for_multibyte_text() {
        let text = "héllo wörld ünïcode ✓ done\nsecond héllo line";
        for width in 1..=30u16 {
            let rows = wrap_rows(text, width);
            for row in &rows {
                assert!(text.is_char_boundary(row.start), "width {width}: start {} not a boundary", row.start);
                assert!(text.is_char_boundary(row.end), "width {width}: end {} not a boundary", row.end);
                let _ = &text[row.clone()];
            }
            // Rows must tile the text in order, without overlap.
            for pair in rows.windows(2) {
                assert!(pair[0].end <= pair[1].start, "width {width}: rows overlap");
            }
        }
    }

    /// Wrapping must never silently drop characters.
    #[test]
    fn wrapping_preserves_every_character() {
        let text = "the quick brown fox\njumps over\n\nthe lazy dog";
        for width in 1..=25u16 {
            let joined: String = wrap_rows(text, width).into_iter().map(|r| &text[r]).collect();
            let expected: String = text.chars().filter(|c| *c != '\n').collect();
            assert_eq!(joined, expected, "width {width} lost or duplicated characters");
        }
    }
}
