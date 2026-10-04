//! Headless libghostty-vt for the SmoothFlow session host (th-5025fb,
//! [ADR-011](../../docs/Decisions/ADR-011-smoothflow-engine-owned-ptys.md)).
//!
//! Every byte a session's agent prints passes through one [`Vt`]. It answers
//! the questions tmux used to (`capture-pane`, `#{alternate_on}`, the pane
//! title, which bytes a key name means) and produces the **snapshot** a client
//! attaching mid-session replays before the live stream: libghostty-vt's own
//! VT formatter output, so it replays exactly on every SmoothFlow client,
//! all of which parse with libghostty-vt at the same pinned commit
//! (`scripts/ghostty-vt/ghostty-vt.lock`).
//!
//! # Snapshots
//!
//! [`Vt::snapshot`] is a byte stream that, fed into a freshly reset terminal of
//! the same size, reproduces this one: the screen, its history, the cursor
//! and its pen, modes, margins, tab stops, charsets, keyboard modes and the
//! pwd, plus the OSC 2 title (which the formatter does not carry) when it
//! takes no more than a quarter of the budget. The palette is left out on
//! purpose: clients theme their own colours.
//!
//! Two things the formatter leaves out are put back here:
//! - **Blank rows at the bottom.** The formatter stops at the last non-blank
//!   row and then places the cursor absolutely, so a screen ending in blank
//!   rows (any shell after `clear`, a TUI that just exited) would replay
//!   shifted, with history rows on the client's screen. The snapshot adds
//!   the missing row breaks between the cells and the trailing state.
//! - **Rows, not lines.** Rows go out as laid out, soft wraps not joined:
//!   joining lets the formatter drop a wrapped row's erased tail, which
//!   shifts every row after it (TUIs that redraw with erase-line over long
//!   lines do that all the time). A client never reflows old wraps itself,
//!   because a resize is followed by a fresh replay (ADR-011).
//!
//! Tested as a property (`tests.rs`): random agent output fed in random
//! chunks, snapshotted and replayed gives the same screen, history, cursor
//! and modes, and the same VT bytes on re-snapshot, with one known
//! libghostty-vt exception: text that wraps at the bottom row under a
//! background colour makes the replay's scroll fill the new row with that
//! colour (BCE), adding invisible coloured blanks after the wrapped row.
//!
//! **The alternate screen.** The formatter only covers the active screen, so
//! while a TUI is up it would lose everything the agent printed before it.
//! `feed` therefore watches for `CSI ? 1049 h` (and `1047`, `47`), splitting the
//! write just before the sequence's final byte, and caches the primary screen
//! at that instant: the primary cannot change while the alternate screen is up.
//! The cache is a second terminal fed the primary's VT snapshot, so it can be
//! resized and bounded like any other. (Being rows, not lines, its soft wraps
//! do not rejoin if the size changes while the TUI is up, and the cursor
//! `?1049l` restores after such a resize follows libghostty-vt's own rule for
//! the saved cursor; the program's next redraw settles both.) While the
//! alternate screen is up a snapshot is the composite
//!
//! ```text
//! <primary snapshot> <the alt-enter sequence the program used> ESC[H ESC[2J <alt snapshot without it>
//! ```
//!
//! The `ESC[H ESC[2J` matters: entering the alternate screen keeps the cursor
//! where the primary replay left it, so without it the alt text lands on the
//! wrong rows. Moving the alt-enter sequence out of the alt snapshot matters
//! too: sent twice, `?1049h` would save the alt cursor over the primary's, and
//! a later `?1049l` would restore the wrong one.
//!
//! **Bounding.** A snapshot never exceeds `max_bytes`, and keeps the newest
//! rows. The visible screen comes first; history fills what is left:
//!
//! 1. The whole screen with all its history, if it fits ([`Fidelity::Full`]).
//! 2. Otherwise the newest history rows that fit, found by a binary search
//!    over the formatter's own row selection (never mid-row, never
//!    mid-escape), plus the full visible screen ([`Fidelity::History`]).
//!    The first kept row may be the tail of a soft-wrapped line.
//! 3. If the visible screen alone is over budget (a pathological row of
//!    combining marks, a tiny budget), its plain text and cursor
//!    ([`Fidelity::Plain`]: no colours, no modes).
//! 4. If even that does not fit, nothing ([`Fidelity::Empty`]): the client has
//!    already reset its terminal, so an empty replay is a blank screen, not a
//!    corrupt one, and the live stream carries on from there.
//!
//! While the alternate screen is up, the alt screen gets the budget first
//! (it is what the user sees) and the cached primary gets the rest, by the
//! same rules ([`Snapshot::primary`]).
//!
//! # Threading
//!
//! A [`Vt`] is `Send` but not `Sync`: one per session, owned by the task
//! that feeds it. Every method takes `&mut self` because libghostty-vt's
//! handle is not safe to read concurrently with anything.

mod alt;
mod ffi;
mod keys;

use alt::{Scanner, Switch};
use ffi::{Format, Term};

/// What the replay preamble for the alternate screen adds besides the
/// program's own enter sequence (which comes out of the alt snapshot).
const ALT_CLEAR: &[u8] = b"\x1b[H\x1b[2J";
/// Used when the alt snapshot had no enter sequence of its own (a fallback).
const ALT_ENTER_DEFAULT: &[u8] = b"\x1b[?1049h";
/// The enter sequences the formatter can emit for the alternate screen.
const ALT_ENTERS: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
/// The most the alternate-screen preamble can add to an alt snapshot.
const ALT_PREAMBLE_MAX: usize = ALT_ENTER_DEFAULT.len() + ALT_CLEAR.len();

/// Why a terminal could not be made or resized.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("terminal size {cols}x{rows} is invalid (both must be at least 1)")]
    InvalidSize { cols: u16, rows: u16 },
    #[error("libghostty-vt could not allocate a terminal")]
    Alloc,
}

/// How much of the terminal a snapshot (or part of one) carries; see the
/// crate docs for the bounding policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// Everything: screen, all history, modes, cursor.
    Full,
    /// The screen and the newest history; `dropped_rows` oldest rows left out.
    History { dropped_rows: usize },
    /// The visible screen's text and the cursor only.
    Plain,
    /// Nothing fit.
    Empty,
}

/// A bounded VT replay of a [`Vt`]; see the crate docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Feed these into a freshly reset terminal of `cols` x `rows`.
    pub bytes: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
    /// The active screen's part.
    pub screen: Fidelity,
    /// While the alternate screen is up, the cached primary's part (`Empty`
    /// when the alternate screen was entered by a sequence `feed` never saw).
    /// `None` on the primary screen.
    pub primary: Option<Fidelity>,
}

/// One session's headless terminal.
#[derive(Debug)]
pub struct Vt {
    term: Term,
    scrollback: usize,
    scanner: Scanner,
    /// While the alternate screen is up: the primary screen as it was when
    /// the alternate went up, replayed into a terminal of its own.
    primary: Option<Term>,
}

impl Vt {
    /// A `cols` x `rows` terminal keeping `scrollback_lines` of history.
    ///
    /// # Errors
    /// [`Error::InvalidSize`] for a zero dimension, [`Error::Alloc`] if
    /// libghostty-vt cannot allocate.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Result<Self, Error> {
        if cols == 0 || rows == 0 {
            return Err(Error::InvalidSize { cols, rows });
        }
        let term = Term::new(cols, rows, scrollback_lines).ok_or(Error::Alloc)?;
        Ok(Self {
            term,
            scrollback: scrollback_lines,
            scanner: Scanner::default(),
            primary: None,
        })
    }

    /// Feed output from the pty. Any bytes are accepted: malformed input is
    /// the program's problem, and libghostty-vt keeps its state consistent.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Fast path: no sequence in flight and no ESC to start one.
        if self.scanner.is_ground() && !bytes.contains(&0x1b) {
            self.term.write(bytes);
            return;
        }
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            match self.scanner.step(b) {
                Some(Switch::Enter) => {
                    // Everything before the final byte: the CSI is not
                    // dispatched yet, so the primary is still on screen.
                    self.term.write(&bytes[start..i]);
                    start = i;
                    if !self.term.alternate_on() {
                        self.primary = self.capture_primary();
                    }
                }
                Some(Switch::Exit) => {
                    self.term.write(&bytes[start..=i]);
                    start = i + 1;
                    self.settle();
                }
                None => {}
            }
        }
        self.term.write(&bytes[start..]);
        self.settle();
    }

    /// Drop the primary cache once the primary screen is back, however it
    /// came back (`?1049l`, a full reset, …).
    fn settle(&mut self) {
        if self.primary.is_some() && !self.term.alternate_on() {
            self.primary = None;
        }
    }

    fn capture_primary(&mut self) -> Option<Term> {
        let (bytes, _) = bounded(&mut self.term, usize::MAX);
        let (_, _, cols, rows) = self.term.geometry();
        let mut copy = Term::new(cols, rows, self.scrollback)?;
        copy.write(&bytes);
        Some(copy)
    }

    /// Resize, reflowing the screen and history (and the cached primary).
    ///
    /// # Errors
    /// [`Error::InvalidSize`] for a zero dimension; the size is unchanged.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        if cols == 0 || rows == 0 || !self.term.resize(cols, rows) {
            return Err(Error::InvalidSize { cols, rows });
        }
        if let Some(primary) = self.primary.as_mut() {
            primary.resize(cols, rows);
        }
        Ok(())
    }

    /// `(cols, rows)`.
    pub fn size(&mut self) -> (u16, u16) {
        let (_, _, cols, rows) = self.term.geometry();
        (cols, rows)
    }

    /// The visible screen as text, what `tmux capture-pane -p` gave: one line
    /// per row (soft wraps not joined), trailing whitespace trimmed per row,
    /// trailing blank rows dropped. The alternate screen while it is up.
    pub fn plain_screen(&mut self) -> String {
        let raw = self.term.format(Format::PlainActive).unwrap_or_default();
        tidy(&String::from_utf8_lossy(&raw))
    }

    /// History and screen as text, what `tmux capture-pane -p -S - -J` gave
    /// (for handoffs and logs, not for replay):
    /// soft-wrapped rows joined, trailing whitespace trimmed, trailing blank
    /// rows dropped. The alternate screen (which has no history) while it is up.
    pub fn plain_scrollback(&mut self) -> String {
        let raw = self.term.format(Format::PlainAll).unwrap_or_default();
        tidy(&String::from_utf8_lossy(&raw))
    }

    /// Is the alternate screen up (tmux's `#{alternate_on}`)?
    pub fn alternate_on(&mut self) -> bool {
        self.term.alternate_on()
    }

    /// The cursor in visible-screen cells, 0-based `(x, y)`.
    pub fn cursor(&mut self) -> (u16, u16) {
        let (x, y, _, _) = self.term.geometry();
        (x, y)
    }

    /// The title set by OSC 0 / OSC 2, if any.
    pub fn title(&mut self) -> Option<String> {
        let t = self.term.title();
        (!t.is_empty()).then(|| String::from_utf8_lossy(&t).into_owned())
    }

    /// Bracketed paste (mode 2004) is on.
    pub fn bracketed_paste(&mut self) -> bool {
        self.term.mode(2004, false) == Some(true)
    }

    /// Application cursor keys (DECCKM, mode 1) are on.
    pub fn cursor_keys_application(&mut self) -> bool {
        self.term.mode(1, false) == Some(true)
    }

    /// A DEC private mode (`CSI ? n h`): `Some(on)`, or `None` for a mode
    /// libghostty-vt does not know. The session host reports mouse tracking
    /// (9, 1000, 1002, 1003), its encoding (1005, 1006, 1015, 1016) and
    /// alternate scroll (1007) this way.
    pub fn dec_mode(&mut self, mode: u16) -> Option<bool> {
        self.term.mode(mode, false)
    }

    /// The Kitty keyboard protocol flags the program pushed; 0 is the legacy
    /// encoding.
    pub fn kitty_keyboard_flags(&mut self) -> u8 {
        self.term.kitty_flags()
    }

    /// No escape sequence or UTF-8 character is half-parsed: a snapshot taken
    /// now can be followed by the next output bytes on a fresh terminal.
    pub fn stream_is_ground(&mut self) -> bool {
        self.term.is_ground()
    }

    /// Bytes the terminal wants written back to the pty since the last call
    /// (answers to device-attribute and status queries).
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.term.take_reply()
    }

    /// The bytes for a tmux-style key name (`Enter`, `C-c`, `Down`, `y`, `F5`,
    /// `M-x`, `BTab`, …), encoded for the program's current input modes:
    /// cursor-key mode, Kitty keyboard flags, modifyOtherKeys. `None` for a
    /// name that is not a key.
    pub fn encode_key(&mut self, name: &str) -> Option<Vec<u8>> {
        let key = keys::parse(name)?;
        let bytes = self.term.encode_key(key.key, key.mods, key.text.as_deref())?;
        (!bytes.is_empty()).then_some(bytes)
    }

    /// Paste text for the pty, as a terminal pastes: wrapped in
    /// `ESC[200~ … ESC[201~` when the program enabled bracketed paste (mode
    /// 2004), else with newlines sent as CRs. Control bytes that could escape
    /// the paste — ESC (so an embedded `ESC[201~` cannot end it early), NUL,
    /// DEL and the like — become spaces either way; tabs and newlines survive.
    pub fn encode_paste(&mut self, text: &str) -> Vec<u8> {
        ffi::encode_paste(text, self.bracketed_paste())
    }

    /// A replay of this terminal no larger than `max_bytes`; see the crate
    /// docs for what it holds and how it is bounded.
    pub fn snapshot(&mut self, max_bytes: usize) -> Snapshot {
        // The formatter does not carry the OSC title; lead with it when the
        // budget has room, since clients show it.
        let title = self
            .title()
            .map(|t| t.chars().filter(|c| !c.is_control()).collect::<String>())
            .filter(|t| !t.is_empty())
            .map(|t| format!("\x1b]2;{t}\x1b\\").into_bytes())
            .filter(|t| t.len() <= max_bytes / 4);
        let mut snap = self.snapshot_screens(max_bytes - title.as_ref().map_or(0, Vec::len));
        if let Some(mut t) = title {
            t.append(&mut snap.bytes);
            snap.bytes = t;
        }
        snap
    }

    fn snapshot_screens(&mut self, max_bytes: usize) -> Snapshot {
        let (cols, rows) = self.size();
        if !self.term.alternate_on() {
            let (bytes, screen) = bounded(&mut self.term, max_bytes);
            return Snapshot {
                bytes,
                cols,
                rows,
                screen,
                primary: None,
            };
        }

        let (alt, screen) = bounded(&mut self.term, max_bytes.saturating_sub(ALT_PREAMBLE_MAX));
        if max_bytes < ALT_PREAMBLE_MAX {
            return Snapshot {
                bytes: Vec::new(),
                cols,
                rows,
                screen: Fidelity::Empty,
                primary: Some(Fidelity::Empty),
            };
        }
        let (enter, alt) = split_alt_enter(alt);
        let tail = enter.len() + ALT_CLEAR.len() + alt.len();
        let (prim, primary) = self
            .primary
            .as_mut()
            .map_or_else(|| (Vec::new(), Fidelity::Empty), |p| bounded(p, max_bytes.saturating_sub(tail)));
        let mut bytes = Vec::with_capacity(prim.len() + tail);
        bytes.extend_from_slice(&prim);
        bytes.extend_from_slice(&enter);
        bytes.extend_from_slice(ALT_CLEAR);
        bytes.extend_from_slice(&alt);
        Snapshot {
            bytes,
            cols,
            rows,
            screen,
            primary: Some(primary),
        }
    }
}

/// Trailing whitespace off every line, trailing blank lines off the end.
fn tidy(text: &str) -> String {
    let mut lines: Vec<&str> = text.split('\n').map(str::trim_end).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Take the first alternate-screen enter sequence out of an alt snapshot.
/// (Cell text never holds ESC, so the first match is the formatter's mode.)
/// A fallback snapshot has none; the default `?1049h` stands in.
fn split_alt_enter(mut alt: Vec<u8>) -> (Vec<u8>, Vec<u8>) {
    for enter in ALT_ENTERS {
        if let Some(at) = alt.windows(enter.len()).position(|w| w == enter) {
            alt.drain(at..at + enter.len());
            return (enter.to_vec(), alt);
        }
    }
    (ALT_ENTER_DEFAULT.to_vec(), alt)
}

/// Where a formatter replay needs patching, measured once per snapshot.
///
/// The VT formatter writes rows separated by CRLF and leaves out blank rows
/// at the bottom of the screen; the extras after the cells then place the
/// cursor absolutely. Replayed as is, a screen whose last rows are blank
/// (any idle shell after `clear`, a TUI that just exited) comes out shifted:
/// the client's screen holds rows that are history on the source. So the
/// replay gets the missing blank rows back as CRLFs between the cells and
/// the trailing extras. The output is `<mode prefix><cells><extras suffix>`;
/// the prefix and suffix do not depend on which rows are selected, so their
/// lengths are measured once against a cells-only format.
struct Layout {
    prefix_len: usize,
    suffix: Vec<u8>,
    /// Rows on the active screen, history included.
    total_rows: usize,
    /// Rows the formatter emits from row 0 (the rest are trailing blanks).
    emitted_rows: usize,
}

impl Layout {
    /// `None` when the output does not have the expected shape; the replay
    /// then goes unpatched (screen content right, rows possibly shifted).
    fn measure(term: &mut Term, full: &[u8]) -> Option<Self> {
        let cells = term.format(Format::VtContent(0))?;
        let rows_text = term.format(Format::PlainRows)?;
        let (_, _, _, rows) = term.geometry();
        let prefix_len = mode_prefix_len(full);
        if full.get(prefix_len..prefix_len + cells.len())? != cells.as_slice() {
            return None;
        }
        let emitted_rows = if rows_text.is_empty() { 0 } else { rows_text.split(|&b| b == b'\n').count() };
        Some(Self {
            prefix_len,
            suffix: full[prefix_len + cells.len()..].to_vec(),
            total_rows: term.scrollback_rows() + usize::from(rows),
            emitted_rows,
        })
    }

    /// CRLFs a replay starting at `from_row` is missing.
    fn missing_newlines(&self, from_row: usize) -> usize {
        if self.emitted_rows > from_row {
            self.total_rows.saturating_sub(self.emitted_rows)
        } else {
            // Nothing but blank rows from here: every row break is missing.
            self.total_rows.saturating_sub(from_row).saturating_sub(1)
        }
    }

    /// Put the missing blank rows back into a replay starting at `from_row`.
    fn patch(&self, mut out: Vec<u8>, from_row: usize) -> Vec<u8> {
        let n = self.missing_newlines(from_row);
        if n == 0 || out.len() < self.prefix_len + self.suffix.len() || !out.ends_with(&self.suffix) {
            return out;
        }
        let at = out.len() - self.suffix.len();
        out.splice(at..at, b"\r\n".repeat(n));
        out
    }
}

/// The leading run of mode sets/resets (`CSI … h` / `CSI … l`) the formatter
/// emits before the cells. Cells start with text or SGR, never with those.
fn mode_prefix_len(b: &[u8]) -> usize {
    let mut i = 0;
    while b.get(i..i + 2) == Some(b"\x1b[") {
        let mut j = i + 2;
        while b.get(j).is_some_and(|c| (0x20..=0x3f).contains(c)) {
            j += 1;
        }
        match b.get(j) {
            Some(b'h' | b'l') => i = j + 1,
            _ => break,
        }
    }
    i
}

/// The bounding policy (crate docs) for one terminal's active screen.
fn bounded(term: &mut Term, max_bytes: usize) -> (Vec<u8>, Fidelity) {
    let Some(full) = term.format(Format::Vt) else {
        return plain_or_empty(term, max_bytes);
    };
    let layout = Layout::measure(term, &full);
    let patch = |out: Vec<u8>, row: u32| match &layout {
        Some(l) => l.patch(out, row as usize),
        None => out,
    };
    let full = patch(full, 0);
    if full.len() <= max_bytes {
        return (full, Fidelity::Full);
    }
    drop(full);
    let history = u32::try_from(term.scrollback_rows()).unwrap_or(u32::MAX);
    let fits = |term: &mut Term, row: u32| term.format(Format::VtFrom(row)).map(|b| patch(b, row)).filter(|b| b.len() <= max_bytes);
    let Some(mut best) = fits(term, history) else {
        return plain_or_empty(term, max_bytes);
    };
    // Invariant: starting at `hi` fits (best holds it); starting at `lo` does
    // not (row 0 is the full snapshot, which did not fit).
    let (mut lo, mut hi) = (0u32, history);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if let Some(b) = fits(term, mid) {
            best = b;
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (best, Fidelity::History { dropped_rows: hi as usize })
}

/// The visible screen's text plus the cursor, or nothing.
fn plain_or_empty(term: &mut Term, max_bytes: usize) -> (Vec<u8>, Fidelity) {
    let raw = term.format(Format::PlainActive).unwrap_or_default();
    let text = String::from_utf8_lossy(&raw);
    let (x, y, _, _) = term.geometry();
    let mut out = text.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\r\n").into_bytes();
    out.extend_from_slice(format!("\x1b[{};{}H", u32::from(y) + 1, u32::from(x) + 1).as_bytes());
    if out.len() <= max_bytes {
        (out, Fidelity::Plain)
    } else {
        (Vec::new(), Fidelity::Empty)
    }
}

#[cfg(test)]
mod tests;
