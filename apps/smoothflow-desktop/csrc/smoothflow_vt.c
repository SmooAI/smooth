// The C side of SmoothFlow Desktop's libghostty-vt bridge (th-872ea8).
//
// libghostty-vt owns the VT state (parser, screens, scrollback, modes); this
// file moves bytes in and a grid snapshot out, the same shape as the Android
// JNI bridge (smooai apps/smoothflow-mobile/android/app/src/main/cpp/
// ghostty_vt_jni.c). Keeping the libghostty structs (sized structs, unions,
// enums) on the C side means Rust only ever sees ints and byte pointers, so
// there is no struct layout to get wrong. src/ghostty.rs is the only Rust that
// calls in, and serializes every call for one handle (it owns it).
//
// The snapshot is ONE uint32_t array, owned by the handle and valid until the
// next call on it:
//
//   header (SF_HEADER words)
//     [0] cols  [1] rows
//     [2] cursor shown (visible AND inside the viewport)  [3] cursor x  [4] cursor y
//     [5] extra-codepoint count (E)  [6] [7] reserved
//   cells (rows * cols * SF_STRIDE words): codepoint, fg, bg, flags
//     fg/bg: SF_COLOR_SET | 0xRRGGBB, or 0 for "the default colour"
//   extras (E words): the grapheme codepoints after each cell's base one, in
//     cell order; flags bits 16..23 say how many belong to that cell.
//
// The flag bits are mirrored in src/ghostty.rs — change both together.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <ghostty/vt.h>

#define SF_HEADER 8
#define SF_STRIDE 4
#define SF_MAX_EXTRA 15
#define SF_COLOR_SET 0x01000000u

#define SF_BOLD (1u << 0)
#define SF_ITALIC (1u << 1)
#define SF_FAINT (1u << 2)
#define SF_INVERSE (1u << 3)
#define SF_INVISIBLE (1u << 4)
#define SF_STRIKE (1u << 5)
#define SF_UNDERLINE (1u << 6)
#define SF_WIDE (1u << 7)
#define SF_SPACER (1u << 8)
#define SF_EXTRA_SHIFT 16

typedef struct SfVt {
    GhosttyTerminal terminal;
    GhosttyRenderState render;
    GhosttyRenderStateRowIterator rows;
    GhosttyRenderStateRowCells cells;
    // Bytes the terminal wants written back to the pty (DA/DSR answers, size
    // reports) since the last sf_vt_reply_clear. Sent upstream as flow.input.
    uint8_t *reply;
    size_t reply_len;
    size_t reply_cap;
    // The last snapshot; grows to the largest one seen and is reused.
    uint32_t *snap;
    size_t snap_cap;
    uint32_t *extras;
    size_t extras_cap;
} SfVt;

static GhosttyColorRgb rgb(uint32_t v) {
    GhosttyColorRgb c = {(uint8_t)(v >> 16), (uint8_t)(v >> 8), (uint8_t)v};
    return c;
}

static uint32_t pack(GhosttyColorRgb c) { return SF_COLOR_SET | ((uint32_t)c.r << 16) | ((uint32_t)c.g << 8) | c.b; }

static void on_write_pty(GhosttyTerminal terminal, void *userdata, const uint8_t *data, size_t len) {
    (void)terminal;
    SfVt *t = (SfVt *)userdata;
    if (!t || !data || len == 0) return;
    if (t->reply_len + len > t->reply_cap) {
        size_t cap = t->reply_cap ? t->reply_cap : 256;
        while (cap < t->reply_len + len) cap *= 2;
        uint8_t *next = realloc(t->reply, cap);
        if (!next) return;
        t->reply = next;
        t->reply_cap = cap;
    }
    memcpy(t->reply + t->reply_len, data, len);
    t->reply_len += len;
}

// DA1/DA2/DA3: answer as Ghostty does (VT220 with ANSI colour). Without a
// callback libghostty-vt ignores the query and a program that waits for the
// answer (vim, some shells' prompt probes) stalls until its timeout.
static bool on_device_attributes(GhosttyTerminal terminal, void *userdata, GhosttyDeviceAttributes *out) {
    (void)terminal;
    (void)userdata;
    if (!out) return false;
    memset(out, 0, sizeof(*out));
    out->primary.conformance_level = 62;
    out->primary.features[0] = 22;
    out->primary.num_features = 1;
    out->secondary.device_type = 1;
    out->secondary.firmware_version = 10;
    out->secondary.rom_cartridge = 0;
    out->tertiary.unit_id = 0;
    return true;
}

void sf_vt_free(SfVt *t) {
    if (!t) return;
    if (t->cells) ghostty_render_state_row_cells_free(t->cells);
    if (t->rows) ghostty_render_state_row_iterator_free(t->rows);
    if (t->render) ghostty_render_state_free(t->render);
    if (t->terminal) ghostty_terminal_free(t->terminal);
    free(t->reply);
    free(t->snap);
    free(t->extras);
    free(t);
}

/// A terminal of `cols` x `rows` with `scrollback` lines of history, on the
/// given theme (`palette` is 256 0xRRGGBB entries). NULL on failure.
SfVt *sf_vt_new(uint16_t cols, uint16_t rows, size_t scrollback, uint32_t fg, uint32_t bg, uint32_t cursor, const uint32_t *palette) {
    if (cols == 0 || rows == 0) return NULL;
    SfVt *t = calloc(1, sizeof(SfVt));
    if (!t) return NULL;
    GhosttyTerminalOptions opts;
    memset(&opts, 0, sizeof(opts));
    opts.cols = cols;
    opts.rows = rows;
    opts.max_scrollback = scrollback;
    if (ghostty_terminal_new(NULL, &t->terminal, opts) != GHOSTTY_SUCCESS || ghostty_render_state_new(NULL, &t->render) != GHOSTTY_SUCCESS ||
        ghostty_render_state_row_iterator_new(NULL, &t->rows) != GHOSTTY_SUCCESS || ghostty_render_state_row_cells_new(NULL, &t->cells) != GHOSTTY_SUCCESS) {
        sf_vt_free(t);
        return NULL;
    }
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_USERDATA, t);
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (const void *)on_write_pty);
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES, (const void *)on_device_attributes);
    GhosttyColorRgb cfg = rgb(fg), cbg = rgb(bg), ccursor = rgb(cursor);
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &cfg);
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &cbg);
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &ccursor);
    if (palette) {
        GhosttyColorRgb p[256];
        for (int i = 0; i < 256; i++) p[i] = rgb(palette[i]);
        ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, p);
    }
    return t;
}

void sf_vt_write(SfVt *t, const uint8_t *data, size_t len) {
    if (!t || !data || len == 0) return;
    ghostty_terminal_vt_write(t->terminal, data, len);
}

/// 0 on success.
int sf_vt_resize(SfVt *t, uint16_t cols, uint16_t rows, uint32_t cell_w, uint32_t cell_h) {
    if (!t || cols == 0 || rows == 0) return -1;
    return ghostty_terminal_resize(t->terminal, cols, rows, cell_w ? cell_w : 1, cell_h ? cell_h : 1) == GHOSTTY_SUCCESS ? 0 : -1;
}

/// The pending reply bytes; valid until the next call on `t`.
const uint8_t *sf_vt_reply(const SfVt *t, size_t *len) {
    if (!t || !len) return NULL;
    *len = t->reply_len;
    return t->reply;
}

void sf_vt_reply_clear(SfVt *t) {
    if (t) t->reply_len = 0;
}

static int push_extra(SfVt *t, size_t *count, uint32_t cp) {
    if (*count >= t->extras_cap) {
        size_t cap = t->extras_cap ? t->extras_cap * 2 : 64;
        uint32_t *next = realloc(t->extras, cap * sizeof(uint32_t));
        if (!next) return 0;
        t->extras = next;
        t->extras_cap = cap;
    }
    t->extras[(*count)++] = cp;
    return 1;
}

/// Snapshot the viewport (format at the top of this file). Returns the words
/// and their count, valid until the next call on `t`; NULL on failure.
const uint32_t *sf_vt_snapshot(SfVt *t, size_t *len) {
    if (!t || !len) return NULL;
    *len = 0;
    if (ghostty_render_state_update(t->render, t->terminal) != GHOSTTY_SUCCESS) return NULL;

    uint16_t cols = 0, rows = 0;
    ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_COLS, &cols);
    ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_ROWS, &rows);
    if (cols == 0 || rows == 0) return NULL;

    size_t cell_words = (size_t)cols * rows * SF_STRIDE;
    if (SF_HEADER + cell_words > t->snap_cap) {
        uint32_t *next = realloc(t->snap, (SF_HEADER + cell_words) * sizeof(uint32_t));
        if (!next) return NULL;
        t->snap = next;
        t->snap_cap = SF_HEADER + cell_words;
    }
    uint32_t *buf = t->snap;
    memset(buf, 0, (SF_HEADER + cell_words) * sizeof(uint32_t));

    bool visible = false, in_view = false, wide_tail = false;
    uint16_t cx = 0, cy = 0;
    ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VISIBLE, &visible);
    ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE, &in_view);
    if (visible && in_view) {
        ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_X, &cx);
        ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_Y, &cy);
        ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_WIDE_TAIL, &wide_tail);
        // On the tail of a wide char the cursor covers the whole char.
        if (wide_tail && cx > 0) cx--;
    }
    buf[0] = cols;
    buf[1] = rows;
    buf[2] = (visible && in_view) ? 1 : 0;
    buf[3] = cx;
    buf[4] = cy;

    size_t extra_count = 0;
    if (ghostty_render_state_get(t->render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &t->rows) == GHOSTTY_SUCCESS) {
        uint16_t y = 0;
        while (y < rows && ghostty_render_state_row_iterator_next(t->rows)) {
            if (ghostty_render_state_row_get(t->rows, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &t->cells) != GHOSTTY_SUCCESS) {
                y++;
                continue;
            }
            uint16_t x = 0;
            while (x < cols && ghostty_render_state_row_cells_next(t->cells)) {
                uint32_t *c = buf + SF_HEADER + ((size_t)y * cols + x) * SF_STRIDE;
                uint32_t flags = 0;

                GhosttyCell raw = 0;
                if (ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, &raw) == GHOSTTY_SUCCESS) {
                    GhosttyCellWide wide = GHOSTTY_CELL_WIDE_NARROW;
                    ghostty_cell_get(raw, GHOSTTY_CELL_DATA_WIDE, &wide);
                    if (wide == GHOSTTY_CELL_WIDE_WIDE) flags |= SF_WIDE;
                    if (wide == GHOSTTY_CELL_WIDE_SPACER_TAIL || wide == GHOSTTY_CELL_WIDE_SPACER_HEAD) flags |= SF_SPACER;
                }

                uint32_t glen = 0;
                ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN, &glen);
                if (glen > 0) {
                    uint32_t cps[SF_MAX_EXTRA + 1];
                    if (glen <= SF_MAX_EXTRA + 1) {
                        ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, cps);
                        c[0] = cps[0];
                        uint32_t extra = 0;
                        for (uint32_t i = 1; i < glen; i++)
                            if (push_extra(t, &extra_count, cps[i])) extra++;
                        flags |= extra << SF_EXTRA_SHIFT;
                    } else {
                        // A grapheme longer than we keep: draw its base codepoint only.
                        uint32_t *heap = malloc(glen * sizeof(uint32_t));
                        if (heap) {
                            ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, heap);
                            c[0] = heap[0];
                            free(heap);
                        }
                    }
                }

                bool has_style = false;
                ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING, &has_style);
                if (has_style) {
                    GhosttyStyle st = GHOSTTY_INIT_SIZED(GhosttyStyle);
                    if (ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &st) == GHOSTTY_SUCCESS) {
                        if (st.bold) flags |= SF_BOLD;
                        if (st.italic) flags |= SF_ITALIC;
                        if (st.faint) flags |= SF_FAINT;
                        if (st.inverse) flags |= SF_INVERSE;
                        if (st.invisible) flags |= SF_INVISIBLE;
                        if (st.strikethrough) flags |= SF_STRIKE;
                        if (st.underline != 0) flags |= SF_UNDERLINE;
                    }
                }
                GhosttyColorRgb fg, bg;
                if (ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, &fg) == GHOSTTY_SUCCESS) c[1] = pack(fg);
                if (ghostty_render_state_row_cells_get(t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR, &bg) == GHOSTTY_SUCCESS) c[2] = pack(bg);
                c[3] = flags;
                x++;
            }
            bool clean = false;
            ghostty_render_state_row_set(t->rows, GHOSTTY_RENDER_STATE_ROW_OPTION_DIRTY, &clean);
            y++;
        }
    }
    GhosttyRenderStateDirty clean_state = GHOSTTY_RENDER_STATE_DIRTY_FALSE;
    ghostty_render_state_set(t->render, GHOSTTY_RENDER_STATE_OPTION_DIRTY, &clean_state);

    buf[5] = (uint32_t)extra_count;
    // Extras go after the cells, in the same buffer, so Rust reads one slice.
    size_t total = SF_HEADER + cell_words + extra_count;
    if (total > t->snap_cap) {
        uint32_t *next = realloc(t->snap, total * sizeof(uint32_t));
        if (!next) {
            buf[5] = 0;
            *len = SF_HEADER + cell_words;
            return t->snap;
        }
        t->snap = next;
        t->snap_cap = total;
    }
    if (extra_count) memcpy(t->snap + SF_HEADER + cell_words, t->extras, extra_count * sizeof(uint32_t));
    *len = total;
    return t->snap;
}
