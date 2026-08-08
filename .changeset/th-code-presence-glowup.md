---
'@smooai/smooth': patch
---

th code: Presence glow-up — one meaning per colour in the TUI

The `th code` TUI had three private colour palettes and one colour doing three
jobs. `markdown.rs` and `tool_diff.rs` each carried their own hardcoded hexes
that duplicated `theme.rs`'s semantics with different values, and inline code
painted a **background fill** — which fights the user's terminal theme, breaks
transparency, and smears into every copy/paste.

Meanwhile brand orange meant "this panel is focused", "type here", *and* "a tool
is running" simultaneously — and it is byte-identical to the amber that is
supposed to be reserved for "Big Smooth needs you", so amber could never mean
anything.

Now: the teal→blue `th` face marks Big Smooth's presence and nothing else, coral
means "act here", amber means "he needs you" and is returned by exactly one
function, and chrome is a warm neutral hairline that carries focus through weight
rather than colour. Tool status also carries a distinct glyph, so state survives
`NO_COLOR` and colourblind vision. Every hardcoded `Color::Rgb` outside
`theme.rs` is gone.
