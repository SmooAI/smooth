// The C side of smooth-flow-vt: the daemon's headless libghostty-vt terminal
// (th-5025fb, ADR-011).
//
// libghostty-vt owns the VT state (parser, screens, scrollback, modes); this
// file moves bytes in and formatter output, mode bits and encoded input out.
// It is the same shape as SmoothFlow Desktop's bridge
// (apps/smoothflow-desktop/csrc/smoothflow_vt.c): every libghostty struct
// (sized structs, unions, enums) stays on this side, so Rust only ever sees
// ints and byte pointers and there is no layout to get wrong. src/ffi.rs is
// the only Rust that calls in, and it owns each handle exclusively.
//
// Nothing here calls back into Rust.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <ghostty/vt.h>

// fvt_format kinds, mirrored in src/ffi.rs.
#define FVT_FORMAT_VT 0       // the active screen: history + screen + extras
#define FVT_FORMAT_VT_FROM 1  // as FVT_FORMAT_VT, from screen row `from_row`
#define FVT_FORMAT_PLAIN_ACTIVE 2 // the visible (active) area as plain text
#define FVT_FORMAT_PLAIN_ALL 3 // history + screen as plain text, soft wraps joined
#define FVT_FORMAT_VT_CONTENT 4 // as FVT_FORMAT_VT_FROM but cells only: no extras
#define FVT_FORMAT_PLAIN_ROWS 5 // history + screen as plain text, one line per row, untrimmed

// fvt_encode_key modifier bits, mirrored in src/ffi.rs.
#define FVT_MOD_SHIFT (1u << 0)
#define FVT_MOD_CTRL (1u << 1)
#define FVT_MOD_ALT (1u << 2)

typedef struct FvtTerm {
    GhosttyTerminal terminal;
    // Bytes the terminal wants written back to the pty (DA answers and the
    // like) since the last fvt_reply_clear.
    uint8_t *reply;
    size_t reply_len;
    size_t reply_cap;
    GhosttyKeyEncoder keys;
    GhosttyKeyEvent key_event;
} FvtTerm;

static void on_write_pty(GhosttyTerminal terminal, void *userdata, const uint8_t *data, size_t len) {
    (void)terminal;
    FvtTerm *t = (FvtTerm *)userdata;
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

// DA1/DA2/DA3: answer as Ghostty does (VT220 with ANSI colour), like the
// desktop bridge. Without it libghostty-vt ignores the query, and a program
// that waits for the answer stalls until its own timeout.
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

void fvt_free(FvtTerm *t) {
    if (!t) return;
    if (t->key_event) ghostty_key_event_free(t->key_event);
    if (t->keys) ghostty_key_encoder_free(t->keys);
    if (t->terminal) ghostty_terminal_free(t->terminal);
    free(t->reply);
    free(t);
}

/// A terminal of `cols` x `rows` keeping `scrollback` lines of history. NULL
/// on failure (including a zero dimension).
FvtTerm *fvt_new(uint16_t cols, uint16_t rows, size_t scrollback) {
    if (cols == 0 || rows == 0) return NULL;
    FvtTerm *t = calloc(1, sizeof(FvtTerm));
    if (!t) return NULL;
    GhosttyTerminalOptions opts;
    memset(&opts, 0, sizeof(opts));
    opts.cols = cols;
    opts.rows = rows;
    opts.max_scrollback = scrollback;
    if (ghostty_terminal_new(NULL, &t->terminal, opts) != GHOSTTY_SUCCESS) {
        fvt_free(t);
        return NULL;
    }
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_USERDATA, t);
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (const void *)on_write_pty);
    ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES, (const void *)on_device_attributes);
    return t;
}

void fvt_write(FvtTerm *t, const uint8_t *data, size_t len) {
    if (!t || !data || len == 0) return;
    ghostty_terminal_vt_write(t->terminal, data, len);
}

/// 0 on success. Cell pixel sizes are 1: nothing headless measures pixels.
int fvt_resize(FvtTerm *t, uint16_t cols, uint16_t rows) {
    if (!t || cols == 0 || rows == 0) return -1;
    return ghostty_terminal_resize(t->terminal, cols, rows, 1, 1) == GHOSTTY_SUCCESS ? 0 : -1;
}

static int grid_ref(GhosttyTerminal terminal, GhosttyPointTag tag, uint16_t x, uint32_t y, GhosttyGridRef *out) {
    GhosttyPoint p;
    memset(&p, 0, sizeof(p));
    p.tag = tag;
    p.value.coordinate.x = x;
    p.value.coordinate.y = y;
    memset(out, 0, sizeof(*out));
    out->size = sizeof(*out);
    return ghostty_terminal_grid_ref(terminal, p, out) == GHOSTTY_SUCCESS ? 0 : -1;
}

/// Format the active screen (see FVT_FORMAT_*). On success returns 0 and an
/// allocation the caller releases with fvt_buf_free; `*out` may be NULL when
/// `*len` is 0.
int fvt_format(FvtTerm *t, int kind, uint32_t from_row, uint8_t **out, size_t *len) {
    if (!t || !out || !len) return -1;
    *out = NULL;
    *len = 0;

    GhosttyFormatterTerminalOptions o;
    memset(&o, 0, sizeof(o));
    o.size = sizeof(o);
    o.extra.size = sizeof(o.extra);
    o.extra.screen.size = sizeof(o.extra.screen);

    GhosttySelection sel;
    memset(&sel, 0, sizeof(sel));
    sel.size = sizeof(sel);
    uint16_t cols = 0, rows = 0;
    ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_COLS, &cols);
    ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_ROWS, &rows);
    if (cols == 0 || rows == 0) return -1;

    switch (kind) {
    case FVT_FORMAT_VT_CONTENT:
        o.emit = GHOSTTY_FORMATTER_FORMAT_VT;
        if (from_row > 0) {
            if (grid_ref(t->terminal, GHOSTTY_POINT_TAG_SCREEN, 0, from_row, &sel.start) != 0) return -1;
            if (grid_ref(t->terminal, GHOSTTY_POINT_TAG_ACTIVE, cols - 1, rows - 1, &sel.end) != 0) return -1;
            o.selection = &sel;
        }
        break;
    case FVT_FORMAT_PLAIN_ROWS:
        o.emit = GHOSTTY_FORMATTER_FORMAT_PLAIN;
        break;
    case FVT_FORMAT_VT:
    case FVT_FORMAT_VT_FROM:
        o.emit = GHOSTTY_FORMATTER_FORMAT_VT;
        // Rows go out exactly as laid out (soft wraps NOT joined). Joining
        // them would let the client re-wrap and keep the wrap flag, but the
        // formatter drops a wrapped row's erased tail when it joins, which
        // shifts every following row: TUIs that redraw with erase-line over
        // long lines hit that constantly. The client never needs to reflow
        // old wraps itself: a resize is followed by a fresh replay from here.
        // The state a fresh terminal needs to continue the stream exactly:
        // modes, margins, tab stops, keyboard modes, the pwd, and the
        // cursor's position, pen, charsets and Kitty keyboard flags.
        // Palette is left out on purpose: clients theme their own colours.
        o.extra.modes = true;
        o.extra.scrolling_region = true;
        o.extra.tabstops = true;
        o.extra.pwd = true;
        o.extra.keyboard = true;
        o.extra.screen.cursor = true;
        o.extra.screen.style = true;
        o.extra.screen.hyperlink = true;
        o.extra.screen.protection = true;
        o.extra.screen.kitty_keyboard = true;
        o.extra.screen.charsets = true;
        if (kind == FVT_FORMAT_VT_FROM && from_row > 0) {
            if (grid_ref(t->terminal, GHOSTTY_POINT_TAG_SCREEN, 0, from_row, &sel.start) != 0) return -1;
            if (grid_ref(t->terminal, GHOSTTY_POINT_TAG_ACTIVE, cols - 1, rows - 1, &sel.end) != 0) return -1;
            o.selection = &sel;
        }
        break;
    case FVT_FORMAT_PLAIN_ACTIVE:
        o.emit = GHOSTTY_FORMATTER_FORMAT_PLAIN;
        o.trim = true;
        if (grid_ref(t->terminal, GHOSTTY_POINT_TAG_ACTIVE, 0, 0, &sel.start) != 0) return -1;
        if (grid_ref(t->terminal, GHOSTTY_POINT_TAG_ACTIVE, cols - 1, rows - 1, &sel.end) != 0) return -1;
        o.selection = &sel;
        break;
    case FVT_FORMAT_PLAIN_ALL:
        o.emit = GHOSTTY_FORMATTER_FORMAT_PLAIN;
        o.trim = true;
        o.unwrap = true;
        break;
    default:
        return -1;
    }

    GhosttyFormatter f = NULL;
    if (ghostty_formatter_terminal_new(NULL, &f, t->terminal, o) != GHOSTTY_SUCCESS) return -1;
    uint8_t *p = NULL;
    size_t n = 0;
    GhosttyResult r = ghostty_formatter_format_alloc(f, NULL, &p, &n);
    ghostty_formatter_free(f);
    if (r != GHOSTTY_SUCCESS) {
        if (p) ghostty_free(NULL, p, n);
        return -1;
    }
    *out = p;
    *len = n;
    return 0;
}

void fvt_buf_free(uint8_t *p, size_t len) {
    if (p) ghostty_free(NULL, p, len);
}

/// 1 when the alternate screen is active, 0 for the primary.
int fvt_alternate_on(FvtTerm *t) {
    if (!t) return 0;
    GhosttyTerminalScreen screen = GHOSTTY_TERMINAL_SCREEN_PRIMARY;
    if (ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen) != GHOSTTY_SUCCESS) return 0;
    return screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE ? 1 : 0;
}

/// The cursor in active-area cells (0-based), and the grid size.
void fvt_geometry(FvtTerm *t, uint16_t *cx, uint16_t *cy, uint16_t *cols, uint16_t *rows) {
    uint16_t x = 0, y = 0, c = 0, r = 0;
    if (t) {
        ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_CURSOR_X, &x);
        ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_CURSOR_Y, &y);
        ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_COLS, &c);
        ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_ROWS, &r);
    }
    if (cx) *cx = x;
    if (cy) *cy = y;
    if (cols) *cols = c;
    if (rows) *rows = r;
}

/// Rows of history above the active area on the active screen.
size_t fvt_scrollback_rows(FvtTerm *t) {
    size_t n = 0;
    if (t) ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS, &n);
    return n;
}

/// A mode's state: 1 set, 0 reset, -1 unknown to libghostty-vt. `ansi` picks
/// ANSI modes (CSI n h) over DEC private ones (CSI ? n h).
int fvt_mode(FvtTerm *t, uint16_t value, int ansi) {
    if (!t) return -1;
    bool on = false;
    if (ghostty_terminal_mode_get(t->terminal, ghostty_mode_new(value, ansi != 0), &on) != GHOSTTY_SUCCESS) return -1;
    return on ? 1 : 0;
}

/// The OSC 0/2 title, borrowed until the next call on `t`; length 0 when unset.
const uint8_t *fvt_title(FvtTerm *t, size_t *len) {
    if (!len) return NULL;
    *len = 0;
    if (!t) return NULL;
    GhosttyString s;
    memset(&s, 0, sizeof(s));
    if (ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_TITLE, &s) != GHOSTTY_SUCCESS) return NULL;
    *len = s.len;
    return s.ptr;
}

/// 1 when the parser and UTF-8 decoder hold no partial sequence.
int fvt_is_ground(FvtTerm *t) { return t && ghostty_terminal_vt_stream_is_ground(t->terminal) ? 1 : 0; }

/// The pending reply bytes; valid until the next call on `t`.
const uint8_t *fvt_reply(const FvtTerm *t, size_t *len) {
    if (!len) return NULL;
    *len = 0;
    if (!t) return NULL;
    *len = t->reply_len;
    return t->reply;
}

void fvt_reply_clear(FvtTerm *t) {
    if (t) t->reply_len = 0;
}

struct named_key {
    const char *name;
    GhosttyKey key;
};

// The logical keys Rust may name (src/keys.rs maps tmux key names onto these).
static const struct named_key KEYS[] = {
    {"enter", GHOSTTY_KEY_ENTER}, {"escape", GHOSTTY_KEY_ESCAPE}, {"tab", GHOSTTY_KEY_TAB},
    {"backspace", GHOSTTY_KEY_BACKSPACE}, {"space", GHOSTTY_KEY_SPACE}, {"up", GHOSTTY_KEY_ARROW_UP},
    {"down", GHOSTTY_KEY_ARROW_DOWN}, {"left", GHOSTTY_KEY_ARROW_LEFT}, {"right", GHOSTTY_KEY_ARROW_RIGHT},
    {"home", GHOSTTY_KEY_HOME}, {"end", GHOSTTY_KEY_END}, {"page_up", GHOSTTY_KEY_PAGE_UP},
    {"page_down", GHOSTTY_KEY_PAGE_DOWN}, {"insert", GHOSTTY_KEY_INSERT}, {"delete", GHOSTTY_KEY_DELETE},
    {"f1", GHOSTTY_KEY_F1}, {"f2", GHOSTTY_KEY_F2}, {"f3", GHOSTTY_KEY_F3}, {"f4", GHOSTTY_KEY_F4},
    {"f5", GHOSTTY_KEY_F5}, {"f6", GHOSTTY_KEY_F6}, {"f7", GHOSTTY_KEY_F7}, {"f8", GHOSTTY_KEY_F8},
    {"f9", GHOSTTY_KEY_F9}, {"f10", GHOSTTY_KEY_F10}, {"f11", GHOSTTY_KEY_F11}, {"f12", GHOSTTY_KEY_F12},
    {"a", GHOSTTY_KEY_A}, {"b", GHOSTTY_KEY_B}, {"c", GHOSTTY_KEY_C}, {"d", GHOSTTY_KEY_D}, {"e", GHOSTTY_KEY_E},
    {"f", GHOSTTY_KEY_F}, {"g", GHOSTTY_KEY_G}, {"h", GHOSTTY_KEY_H}, {"i", GHOSTTY_KEY_I}, {"j", GHOSTTY_KEY_J},
    {"k", GHOSTTY_KEY_K}, {"l", GHOSTTY_KEY_L}, {"m", GHOSTTY_KEY_M}, {"n", GHOSTTY_KEY_N}, {"o", GHOSTTY_KEY_O},
    {"p", GHOSTTY_KEY_P}, {"q", GHOSTTY_KEY_Q}, {"r", GHOSTTY_KEY_R}, {"s", GHOSTTY_KEY_S}, {"t", GHOSTTY_KEY_T},
    {"u", GHOSTTY_KEY_U}, {"v", GHOSTTY_KEY_V}, {"w", GHOSTTY_KEY_W}, {"x", GHOSTTY_KEY_X}, {"y", GHOSTTY_KEY_Y},
    {"z", GHOSTTY_KEY_Z}, {"0", GHOSTTY_KEY_DIGIT_0}, {"1", GHOSTTY_KEY_DIGIT_1}, {"2", GHOSTTY_KEY_DIGIT_2},
    {"3", GHOSTTY_KEY_DIGIT_3}, {"4", GHOSTTY_KEY_DIGIT_4}, {"5", GHOSTTY_KEY_DIGIT_5}, {"6", GHOSTTY_KEY_DIGIT_6},
    {"7", GHOSTTY_KEY_DIGIT_7}, {"8", GHOSTTY_KEY_DIGIT_8}, {"9", GHOSTTY_KEY_DIGIT_9}, {"`", GHOSTTY_KEY_BACKQUOTE},
    {"\\", GHOSTTY_KEY_BACKSLASH}, {"[", GHOSTTY_KEY_BRACKET_LEFT}, {"]", GHOSTTY_KEY_BRACKET_RIGHT},
    {",", GHOSTTY_KEY_COMMA}, {"=", GHOSTTY_KEY_EQUAL}, {"-", GHOSTTY_KEY_MINUS}, {".", GHOSTTY_KEY_PERIOD},
    {"'", GHOSTTY_KEY_QUOTE}, {";", GHOSTTY_KEY_SEMICOLON}, {"/", GHOSTTY_KEY_SLASH},
    {"", GHOSTTY_KEY_UNIDENTIFIED},
};

/// Encode one key press the way the program asked (cursor-key mode, Kitty
/// flags, modifyOtherKeys and the rest come from the terminal's state).
/// `name` is a logical key from KEYS (its length in `name_len`); `utf8` is
/// the unmodified text the key types, or NULL. Returns the encoded length,
/// which is larger than `cap` when `out` was too small (nothing usable was
/// written then), or (size_t)-1 for an unknown key name or an encoder error.
size_t fvt_encode_key(FvtTerm *t, const char *name, size_t name_len, uint32_t mods, const char *utf8, size_t utf8_len, uint8_t *out, size_t cap) {
    if (!t || !name) return (size_t)-1;
    GhosttyKey key = GHOSTTY_KEY_UNIDENTIFIED;
    bool found = false;
    for (size_t i = 0; i < sizeof(KEYS) / sizeof(KEYS[0]); i++) {
        if (strlen(KEYS[i].name) == name_len && memcmp(KEYS[i].name, name, name_len) == 0) {
            key = KEYS[i].key;
            found = true;
            break;
        }
    }
    if (!found) return (size_t)-1;
    if (!t->keys && ghostty_key_encoder_new(NULL, &t->keys) != GHOSTTY_SUCCESS) return (size_t)-1;
    if (!t->key_event && ghostty_key_event_new(NULL, &t->key_event) != GHOSTTY_SUCCESS) return (size_t)-1;

    ghostty_key_encoder_setopt_from_terminal(t->keys, t->terminal);
    // tmux's M-x is ESC x whatever DEC 1036 says; xterm-alike programs expect
    // that too. (Kitty mode encodes Alt itself and ignores this.)
    bool esc_prefix = true;
    ghostty_key_encoder_setopt(t->keys, GHOSTTY_KEY_ENCODER_OPT_ALT_ESC_PREFIX, &esc_prefix);
    // There is no keyboard layout here: Alt is Alt, never macOS Option
    // composing a character (setopt_from_terminal resets this to false).
    GhosttyOptionAsAlt as_alt = GHOSTTY_OPTION_AS_ALT_TRUE;
    ghostty_key_encoder_setopt(t->keys, GHOSTTY_KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT, &as_alt);

    GhosttyMods m = 0;
    if (mods & FVT_MOD_SHIFT) m |= GHOSTTY_MODS_SHIFT;
    if (mods & FVT_MOD_CTRL) m |= GHOSTTY_MODS_CTRL;
    if (mods & FVT_MOD_ALT) m |= GHOSTTY_MODS_ALT;
    ghostty_key_event_set_action(t->key_event, GHOSTTY_KEY_ACTION_PRESS);
    ghostty_key_event_set_key(t->key_event, key);
    ghostty_key_event_set_mods(t->key_event, m);
    // Shift is spent producing the text ("Y" for shift+y), as a keyboard
    // layout would report it.
    ghostty_key_event_set_consumed_mods(t->key_event, (utf8 && utf8_len) ? (m & GHOSTTY_MODS_SHIFT) : 0);
    ghostty_key_event_set_composing(t->key_event, false);
    ghostty_key_event_set_utf8(t->key_event, utf8_len ? utf8 : NULL, utf8_len);
    uint32_t cp = 0;
    if (utf8 && utf8_len == 1) cp = (uint8_t)utf8[0];
    if (cp >= 'A' && cp <= 'Z') cp = cp - 'A' + 'a';
    ghostty_key_event_set_unshifted_codepoint(t->key_event, cp);

    // Ctrl+letter outside the Kitty protocol is the C0 byte, as tmux and
    // xterm send it. Ghostty's encoder reports Ctrl+I/M as CSI u even then
    // (to tell them from Tab/Enter), which a program that never asked for
    // the Kitty protocol may not read.
    uint8_t kitty = 0;
    ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS, &kitty);
    if (kitty == 0 && m == GHOSTTY_MODS_CTRL && utf8 && utf8_len == 1 && utf8[0] >= 'a' && utf8[0] <= 'z') {
        if (cap >= 1) out[0] = (uint8_t)(utf8[0] & 0x1f);
        return 1;
    }

    size_t n = 0;
    GhosttyResult r = ghostty_key_encoder_encode(t->keys, t->key_event, (char *)out, cap, &n);
    if (r == GHOSTTY_SUCCESS) return n;
    if (r == GHOSTTY_OUT_OF_SPACE) return n > cap ? n : cap + 1;
    return (size_t)-1;
}

/// Encode a paste for the pty: unsafe control bytes (ESC included, so an
/// embedded ESC[201~ cannot end the paste early) become spaces, and the text
/// is wrapped in ESC[200~ … ESC[201~ when `bracketed`, else its newlines
/// become CRs. `data` is rewritten in place. Returns the encoded length, which
/// is larger than `cap` when `out` was too small, or (size_t)-1 on error.
size_t fvt_encode_paste(char *data, size_t len, int bracketed, uint8_t *out, size_t cap) {
    size_t n = 0;
    GhosttyResult r = ghostty_paste_encode(data, len, bracketed != 0, (char *)out, cap, &n);
    if (r == GHOSTTY_SUCCESS) return n;
    if (r == GHOSTTY_OUT_OF_SPACE) return n > cap ? n : cap + 1;
    return (size_t)-1;
}
