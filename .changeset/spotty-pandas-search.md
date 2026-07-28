---
'@smooai/smooth': patch
---

The message composer now grows with what you type, in both `th code` and Big Smooth.

**`th code`** — the input was one clipped row: anything past the box width was
invisible, `\n` was rewritten to a space on typing and paste, and the cursor was
placed by adding a raw *byte* offset to the box origin, so it drifted on any
multi-byte character. It now wraps, grows to six rows, and scrolls beyond that.

- New `composer` module holds the wrap arithmetic — rows, cursor row/column, and
  scroll clamping — so the renderer and the height calculation can't disagree
  about where a row ends. Pure and unit-tested, including a property-style check
  that every wrapped row is a valid `char` boundary at every width.
- Multi-line paste keeps its structure. Line endings are normalized over the
  whole pasted string because terminals disagree about the separator (tmux sends
  bare `\r`, Unix `\n`, Windows `\r\n`) and CRLF can't be collapsed one `char` at
  a time.
- Mouse capture is enabled **only while the draft is taller than the box**, and
  released the moment it fits again. The TUI keeps finalized chat in the
  terminal's own scrollback, so holding capture for the whole session would cost
  native wheel-scroll and drag-select permanently; scoping it to "there's a long
  draft open" keeps the default interaction intact. It is also released
  unconditionally on exit.
- Once you scroll by hand the view stays put until you type again, instead of
  snapping back to the cursor every frame.
- The box borrows rows from the streaming preview, which is clamped so at least
  one preview row survives — a long draft can't hide the answer you're replying to.

**Big Smooth** — the composer had `max-h-40` but `rows={1}` pinned it to one
line, so the cap was unreachable. It now uses CSS `field-sizing-content` (with a
JS fallback for browsers without it) to grow to that cap and scroll past it.
