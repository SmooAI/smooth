---
'smooai-smooth-cli': minor
'smooai-smooth-code': minor
---

Visual glow-up — "Smooth Flow" design language across the `th` CLI + `th code` TUI.

The brand is a color that flows warm→cool; the chrome now makes that literal.

- **Flow rule (the signature):** `flow_rule(width, ch)` renders a horizontal
  hairline whose every cell steps the full Smooth gradient (orange→pink→teal→
  blue) — the wordmark stretched into a divider. Added to both `gradient.rs`
  (CLI, ANSI) and `theme.rs` (TUI, ratatui spans). Used under the `th up` boot
  header; reserved for headers so it reads as special.
- **Curated glyph vocabulary** (one set, used everywhere): user `❯` (warm),
  agent `✦` + the brand wordmark (cool), tool `▸`→`✓`/`✗`, system `·`, stream
  cursor `▌`. Replaces the ad-hoc `⚙`/`⏳`/`█` mix in the live inline renderer.
- `flow_color` interpolates the 4-stop warm→cool brand gradient; all new
  helpers unit-tested.
