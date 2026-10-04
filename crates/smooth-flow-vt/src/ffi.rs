//! The only module with `unsafe`: the C bridge's functions (`csrc/flow_vt.c`)
//! and [`Term`], which owns one bridge handle, frees it on drop, and copies
//! every pointer/length pair the C side returns into owned Rust values before
//! the next call can invalidate it. The C side never calls back into Rust, so
//! nothing here can unwind across the boundary.
#![allow(unsafe_code)]

use std::ptr::NonNull;

#[repr(C)]
struct FvtTerm {
    _private: [u8; 0],
}

extern "C" {
    fn fvt_new(cols: u16, rows: u16, scrollback: usize) -> *mut FvtTerm;
    fn fvt_free(t: *mut FvtTerm);
    fn fvt_write(t: *mut FvtTerm, data: *const u8, len: usize);
    fn fvt_resize(t: *mut FvtTerm, cols: u16, rows: u16) -> i32;
    fn fvt_format(t: *mut FvtTerm, kind: i32, from_row: u32, out: *mut *mut u8, len: *mut usize) -> i32;
    fn fvt_buf_free(p: *mut u8, len: usize);
    fn fvt_alternate_on(t: *mut FvtTerm) -> i32;
    fn fvt_geometry(t: *mut FvtTerm, cx: *mut u16, cy: *mut u16, cols: *mut u16, rows: *mut u16);
    fn fvt_scrollback_rows(t: *mut FvtTerm) -> usize;
    fn fvt_mode(t: *mut FvtTerm, value: u16, ansi: i32) -> i32;
    fn fvt_kitty_flags(t: *mut FvtTerm) -> i32;
    fn fvt_title(t: *mut FvtTerm, len: *mut usize) -> *const u8;
    fn fvt_is_ground(t: *mut FvtTerm) -> i32;
    fn fvt_reply(t: *const FvtTerm, len: *mut usize) -> *const u8;
    fn fvt_reply_clear(t: *mut FvtTerm);
    fn fvt_encode_key(t: *mut FvtTerm, name: *const u8, name_len: usize, mods: u32, utf8: *const u8, utf8_len: usize, out: *mut u8, cap: usize) -> usize;
    fn fvt_encode_paste(data: *mut u8, len: usize, bracketed: i32, out: *mut u8, cap: usize) -> usize;
}

/// `fvt_format` kinds (csrc/flow_vt.c `FVT_FORMAT_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// VT replay of the active screen: history, screen and the extras.
    Vt,
    /// [`Format::Vt`] starting at this screen row (0 = oldest history row).
    VtFrom(u32),
    /// The visible area as plain text, trailing whitespace trimmed per row.
    PlainActive,
    /// History and screen as plain text, soft-wrapped rows joined.
    PlainAll,
    /// [`Format::VtFrom`]'s cells alone, without any of the extras.
    VtContent(u32),
    /// History and screen as plain text, one line per row (wraps not
    /// joined, nothing trimmed): its line count is the formatter's row count.
    PlainRows,
}

/// Modifier bits for [`Term::encode_key`] (csrc/flow_vt.c `FVT_MOD_*`).
pub mod mods {
    pub const SHIFT: u32 = 1 << 0;
    pub const CTRL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
}

/// Encoded input is short; anything longer is re-requested at its real size.
const INLINE_CAP: usize = 64;

/// One libghostty-vt terminal (with its reply buffer and key encoder).
#[derive(Debug)]
pub struct Term {
    raw: NonNull<FvtTerm>,
}

// SAFETY: a `Term` owns its handle exclusively (no Clone, `&mut self` for
// every mutation) and libghostty-vt keeps no thread-local state, so moving
// the whole terminal to another thread is sound. It is deliberately not
// `Sync`: the C side has interior state (reply buffer, encoder scratch) that
// even `&self` reads must not race on, which is why the readers below take
// `&mut self` where the C function mutates.
unsafe impl Send for Term {}

impl Term {
    /// `None` for a zero dimension or an allocation failure.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Option<Self> {
        // SAFETY: plain values in; a null return is handled.
        let raw = unsafe { fvt_new(cols, rows, scrollback) };
        NonNull::new(raw).map(|raw| Self { raw })
    }

    pub fn write(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // SAFETY: the handle is live; `data` is a valid slice for the call.
        unsafe { fvt_write(self.raw.as_ptr(), data.as_ptr(), data.len()) }
    }

    /// False when libghostty-vt refused the size (zero dimension).
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        // SAFETY: the handle is live.
        unsafe { fvt_resize(self.raw.as_ptr(), cols, rows) == 0 }
    }

    /// The formatter's output, or `None` if it failed (e.g. a row out of range).
    pub fn format(&mut self, kind: Format) -> Option<Vec<u8>> {
        let (k, from) = match kind {
            Format::Vt => (0, 0),
            Format::VtFrom(row) => (1, row),
            Format::PlainActive => (2, 0),
            Format::PlainAll => (3, 0),
            Format::VtContent(row) => (4, row),
            Format::PlainRows => (5, 0),
        };
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        // SAFETY: the handle is live; the out-pointers point at locals.
        let rc = unsafe { fvt_format(self.raw.as_ptr(), k, from, &raw mut ptr, &raw mut len) };
        if rc != 0 {
            return None;
        }
        if ptr.is_null() || len == 0 {
            // SAFETY: freeing (ptr, len) exactly as returned; null is a no-op.
            unsafe { fvt_buf_free(ptr, len) };
            return Some(Vec::new());
        }
        // SAFETY: on success the bridge returns `len` initialized bytes at
        // `ptr`, owned by us until fvt_buf_free. Copy, then free once.
        let out = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
        // SAFETY: as above; this is the one free of that allocation.
        unsafe { fvt_buf_free(ptr, len) };
        Some(out)
    }

    pub fn alternate_on(&mut self) -> bool {
        // SAFETY: the handle is live.
        unsafe { fvt_alternate_on(self.raw.as_ptr()) == 1 }
    }

    /// `(cursor_x, cursor_y, cols, rows)`.
    pub fn geometry(&mut self) -> (u16, u16, u16, u16) {
        let (mut x, mut y, mut c, mut r) = (0u16, 0u16, 0u16, 0u16);
        // SAFETY: the handle is live; the out-pointers point at locals.
        unsafe { fvt_geometry(self.raw.as_ptr(), &raw mut x, &raw mut y, &raw mut c, &raw mut r) };
        (x, y, c, r)
    }

    pub fn scrollback_rows(&mut self) -> usize {
        // SAFETY: the handle is live.
        unsafe { fvt_scrollback_rows(self.raw.as_ptr()) }
    }

    /// `Some(on)` for a mode libghostty-vt knows, `None` otherwise.
    pub fn mode(&mut self, value: u16, ansi: bool) -> Option<bool> {
        // SAFETY: the handle is live.
        match unsafe { fvt_mode(self.raw.as_ptr(), value, i32::from(ansi)) } {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }
    }

    /// The Kitty keyboard flags in effect (0 when the program pushed none).
    pub fn kitty_flags(&mut self) -> u8 {
        // SAFETY: the handle is live.
        u8::try_from(unsafe { fvt_kitty_flags(self.raw.as_ptr()) }).unwrap_or(0)
    }

    /// The OSC 0/2 title bytes (empty when unset).
    pub fn title(&mut self) -> Vec<u8> {
        let mut len = 0usize;
        // SAFETY: the handle is live; `len` is a local.
        let ptr = unsafe { fvt_title(self.raw.as_ptr(), &raw mut len) };
        if ptr.is_null() || len == 0 {
            return Vec::new();
        }
        // SAFETY: the bridge returns `len` bytes borrowed until the next call
        // on this handle; we copy them before making one.
        unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
    }

    pub fn is_ground(&mut self) -> bool {
        // SAFETY: the handle is live.
        unsafe { fvt_is_ground(self.raw.as_ptr()) == 1 }
    }

    /// Drain the bytes the terminal wants written back to the pty.
    pub fn take_reply(&mut self) -> Vec<u8> {
        let mut len = 0usize;
        // SAFETY: the handle is live; `len` is a local.
        let ptr = unsafe { fvt_reply(self.raw.as_ptr(), &raw mut len) };
        let out = if ptr.is_null() || len == 0 {
            Vec::new()
        } else {
            // SAFETY: `len` bytes owned by the handle, valid until the next
            // call on it; copied before the clear below.
            unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
        };
        // SAFETY: the handle is live.
        unsafe { fvt_reply_clear(self.raw.as_ptr()) };
        out
    }

    /// Encode one key press against the terminal's current input modes.
    /// `None` when `key` is not one of the bridge's logical key names.
    pub fn encode_key(&mut self, key: &str, mods: u32, utf8: Option<&str>) -> Option<Vec<u8>> {
        let text = utf8.unwrap_or("");
        let mut buf = vec![0u8; INLINE_CAP];
        loop {
            // SAFETY: the handle is live; every pointer/length pair describes
            // a live slice for the duration of the call.
            let n = unsafe {
                fvt_encode_key(
                    self.raw.as_ptr(),
                    key.as_ptr(),
                    key.len(),
                    mods,
                    text.as_ptr(),
                    text.len(),
                    buf.as_mut_ptr(),
                    buf.len(),
                )
            };
            if n == usize::MAX {
                return None;
            }
            if n <= buf.len() {
                buf.truncate(n);
                return Some(buf);
            }
            buf.resize(n, 0);
        }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        // SAFETY: the handle came from fvt_new and is freed exactly once.
        unsafe { fvt_free(self.raw.as_ptr()) }
    }
}

/// Encode paste text for the pty (see `fvt_encode_paste`).
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    // The bridge rewrites its input in place (unsafe bytes → spaces).
    let mut data = text.as_bytes().to_vec();
    // Room for the data plus both bracket markers; grown if the bridge asks.
    let mut out = vec![0u8; data.len() + 16];
    loop {
        // SAFETY: both pointer/length pairs describe live, exclusively
        // borrowed buffers for the duration of the call.
        let n = unsafe { fvt_encode_paste(data.as_mut_ptr(), data.len(), i32::from(bracketed), out.as_mut_ptr(), out.len()) };
        if n == usize::MAX {
            return Vec::new();
        }
        if n <= out.len() {
            out.truncate(n);
            return out;
        }
        // The first pass already sanitized `data` in place; re-encoding the
        // sanitized bytes gives the same result.
        out.resize(n, 0);
    }
}
