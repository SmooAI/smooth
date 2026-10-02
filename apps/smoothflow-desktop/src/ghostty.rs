//! libghostty-vt, wrapped safely (th-872ea8). The only module in the crate
//! with `unsafe`.
//!
//! Rust never touches libghostty-vt's structs: `csrc/smoothflow_vt.c` (the
//! same shape as the Android app's JNI bridge) owns the terminal, the render
//! state and the reply buffer, and hands back plain words and bytes. This
//! module declares those few C functions and wraps them in [`Vt`], which owns
//! one handle, frees it on drop, and checks every pointer and length the C
//! side returns. Nothing here can unwind into C: the C side calls no Rust.

use std::ptr::NonNull;

#[repr(C)]
struct SfVt {
    _private: [u8; 0],
}

extern "C" {
    fn sf_vt_new(cols: u16, rows: u16, scrollback: usize, fg: u32, bg: u32, cursor: u32, palette: *const u32) -> *mut SfVt;
    fn sf_vt_free(t: *mut SfVt);
    fn sf_vt_write(t: *mut SfVt, data: *const u8, len: usize);
    fn sf_vt_resize(t: *mut SfVt, cols: u16, rows: u16, cell_w: u32, cell_h: u32) -> i32;
    fn sf_vt_reply(t: *const SfVt, len: *mut usize) -> *const u8;
    fn sf_vt_reply_clear(t: *mut SfVt);
    fn sf_vt_snapshot(t: *mut SfVt, len: *mut usize) -> *const u32;
}

// Snapshot layout — mirrors csrc/smoothflow_vt.c; change both together.
const HEADER: usize = 8;
const STRIDE: usize = 4;
const COLOR_SET: u32 = 0x0100_0000;

/// Cell flag bits (csrc/smoothflow_vt.c `SF_*`).
pub mod flag {
    pub const BOLD: u32 = 1 << 0;
    pub const ITALIC: u32 = 1 << 1;
    pub const FAINT: u32 = 1 << 2;
    pub const INVERSE: u32 = 1 << 3;
    pub const INVISIBLE: u32 = 1 << 4;
    pub const STRIKE: u32 = 1 << 5;
    pub const UNDERLINE: u32 = 1 << 6;
    pub const WIDE: u32 = 1 << 7;
    pub const SPACER: u32 = 1 << 8;
    pub const EXTRA_SHIFT: u32 = 16;
}

/// The theme a terminal starts with: default colours and the 256 palette.
pub struct Theme<'a> {
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub palette: &'a [u32; 256],
}

/// One cell of a [`Snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The base codepoint; `0` for an empty cell.
    pub codepoint: u32,
    /// `0xRRGGBB`, or `None` for the terminal's default.
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub flags: u32,
}

/// The viewport as libghostty-vt rendered it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    /// `(row, col)` when the cursor is visible and in the viewport; on a wide
    /// char it is the char's first cell.
    pub cursor: Option<(usize, usize)>,
    /// `rows * cols` cells, row-major.
    pub cells: Vec<Cell>,
    /// Each cell's grapheme codepoints after its base one (combining marks,
    /// ZWJ sequences), aligned with `cells`; most are empty.
    pub extras: Vec<Vec<char>>,
}

impl Snapshot {
    #[must_use]
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        (row < self.rows && col < self.cols).then(|| &self.cells[row * self.cols + col])
    }
}

/// One libghostty-vt terminal.
pub struct Vt {
    ptr: NonNull<SfVt>,
}

// SAFETY: the handle is plain heap state with no thread affinity (no
// thread-locals, no callbacks into Rust); `&mut self` on every mutating call
// serializes access, and `Vt` is not `Sync`.
unsafe impl Send for Vt {}

impl Vt {
    /// A `cols` x `rows` terminal (both at least 1) with `scrollback` lines of
    /// history. `None` when libghostty-vt cannot allocate one.
    #[must_use]
    pub fn new(cols: u16, rows: u16, scrollback: usize, theme: &Theme<'_>) -> Option<Self> {
        // SAFETY: plain values and a pointer to 256 u32s that outlives the
        // call; the C side copies the palette.
        let ptr = unsafe {
            sf_vt_new(
                cols.max(1),
                rows.max(1),
                scrollback,
                theme.foreground,
                theme.background,
                theme.cursor,
                theme.palette.as_ptr(),
            )
        };
        NonNull::new(ptr).map(|ptr| Self { ptr })
    }

    /// Feed pty output.
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // SAFETY: `ptr` is live (owned by self); the slice is valid for its length.
        unsafe { sf_vt_write(self.ptr.as_ptr(), bytes.as_ptr(), bytes.len()) }
    }

    /// Resize; `false` when libghostty-vt refused.
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        // SAFETY: `ptr` is live; plain values.
        unsafe { sf_vt_resize(self.ptr.as_ptr(), cols.max(1), rows.max(1), 1, 1) == 0 }
    }

    /// Take the bytes the terminal wants written back to the pty (DA/DSR
    /// answers, size reports): `flow.input` for the session.
    pub fn take_replies(&mut self) -> Vec<u8> {
        let mut len = 0usize;
        // SAFETY: `ptr` is live; `len` is a valid out-pointer. The returned
        // buffer is owned by the handle and stays valid until the next call
        // on it, so it is copied before `sf_vt_reply_clear`.
        unsafe {
            let data = sf_vt_reply(self.ptr.as_ptr(), &raw mut len);
            if data.is_null() || len == 0 {
                return Vec::new();
            }
            let out = std::slice::from_raw_parts(data, len).to_vec();
            sf_vt_reply_clear(self.ptr.as_ptr());
            out
        }
    }

    /// Render the viewport.
    pub fn snapshot(&mut self) -> Snapshot {
        let mut len = 0usize;
        // SAFETY: `ptr` is live; `len` is a valid out-pointer. The words are
        // owned by the handle and valid until the next call on it; they are
        // copied out before this borrow of `self` ends.
        let words = unsafe {
            let data = sf_vt_snapshot(self.ptr.as_ptr(), &raw mut len);
            if data.is_null() || len < HEADER {
                return Snapshot::default();
            }
            std::slice::from_raw_parts(data, len)
        };
        decode(words)
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `sf_vt_new` and is freed exactly once.
        unsafe { sf_vt_free(self.ptr.as_ptr()) }
    }
}

/// Parse the snapshot words (bounds-checked: a short buffer yields what fits).
fn decode(words: &[u32]) -> Snapshot {
    if words.len() < HEADER {
        return Snapshot::default();
    }
    let cols = words[0] as usize;
    let rows = words[1] as usize;
    let n = cols * rows;
    let Some(cell_words) = words.get(HEADER..HEADER + n * STRIDE) else {
        return Snapshot::default();
    };
    let extra_count = words[5] as usize;
    let extra_words = words.get(HEADER + n * STRIDE..HEADER + n * STRIDE + extra_count).unwrap_or(&[]);
    let color = |w: u32| (w & COLOR_SET != 0).then_some(w & 0x00ff_ffff);
    let mut cells = Vec::with_capacity(n);
    let mut extras = Vec::with_capacity(n);
    let mut next_extra = 0usize;
    for c in cell_words.as_chunks::<STRIDE>().0 {
        let flags = c[3];
        let count = ((flags >> flag::EXTRA_SHIFT) & 0xff) as usize;
        let mine = extra_words.get(next_extra..next_extra + count).unwrap_or(&[]);
        next_extra += count;
        extras.push(mine.iter().filter_map(|cp| char::from_u32(*cp)).collect());
        cells.push(Cell {
            codepoint: c[0],
            fg: color(c[1]),
            bg: color(c[2]),
            flags,
        });
    }
    let cursor = (words[2] != 0)
        .then(|| (words[4] as usize, words[3] as usize))
        .filter(|(r, c)| *r < rows && *c < cols);
    Snapshot {
        cols,
        rows,
        cursor,
        cells,
        extras,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_survives_a_short_buffer() {
        assert_eq!(decode(&[3, 2, 0, 0, 0, 0, 0, 0, 1]), Snapshot::default());
        let mut words = vec![1, 1, 1, 0, 0, 1, 0, 0, u32::from('e'), COLOR_SET | 0x00ff_0000, 0, 1 << flag::EXTRA_SHIFT];
        words.push(0x301);
        let s = decode(&words);
        assert_eq!(s.cursor, Some((0, 0)));
        assert_eq!(s.cells[0].fg, Some(0xff0000));
        assert_eq!(s.cells[0].bg, None);
        assert_eq!(s.extras[0], vec!['\u{301}']);
    }
}
