---
"@smooai/smooth": patch
---

TUI: drop "Coding", use the brand gradient for the wordmark.

- "Smooth Coding" → "Smooth" everywhere user-visible (chat panel
  title, welcome message, doc strings). The product's name is
  "Smooth" — this is the coding surface of it, not a separate
  product.
- New `theme::smooth_wordmark()` returns a `Vec<Span<'static>>`
  rendering "Smooth" with the same per-character gradient the CLI
  uses in `gradient::smooth()`:
    * `Smoo`  →  #f49f0a orange → #ff6b6c pink (linear over 4 chars)
    * `th`    →  #00a6a6 teal   → #1238dd blue (linear over 2 chars)
  The chat panel border title now uses it, so the wordmark in the
  TUI matches the `th` CLI banner and the horizontal logo.
