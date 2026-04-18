---
"@smooai/smooth": patch
---

TUI colors: orange is the primary frame accent, green is a
secondary accent on assistant labels and the banner gradient.
Previously panel borders + the chat title used green, which made
the input-box border blend with assistant labels — users reported
they couldn't see where to type.

- `panel_border(true)` → Smoo AI orange (`#f49f0a`), was green.
- `title()` → orange, was green.
- New `input_border(mode)` helper: the message-input panel gets an
  orange bold border in input mode and a gray border only when the
  user explicitly escapes into normal mode. The chat panel follows
  focus; the input panel stays obvious as "the place to type."
- New "▶ Message" title on the input panel, orange + bold.
- Assistant labels stay green (secondary accent), user labels stay
  orange (primary accent), banner keeps the orange→green vertical
  gradient — green lives on as the destination color.

All colors verified against
`smooai/packages/ui/globals.css` (the canonical palette): orange
`#f49f0a`, green `#00a6a6`, red `#ff6b6c`, blue `#bbdef0`,
gray-700 `#4e4e4e` all match.

Regression test: `test_input_border_is_orange_in_input_mode_gray_in_normal`.
