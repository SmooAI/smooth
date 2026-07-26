---
'@smooai/smooth': patch
---

`th code`: first application of the smooth-glow-up (Presence) skill
(pearl th-24cdf3).

- **Greeting printed twice** on every cold start — the splash renders "Type a
  message to get started. /help for commands." and `app.rs` added an identical
  `System:` message directly beneath it. Removed the duplicate.
- **Status bar hierarchy.** It was flat pipe-soup — identity, metrics and
  keybindings in one uniform gray separated by `|`, with the health dot buried
  mid-row. Now presence leads (the health glyph opens the line — it's Big
  Smooth's "I'm awake" signal), live state reads next, and the static
  keybindings are dimmed and right-aligned so alignment does the separating
  work. Middots replace pipes throughout.
- **Health state no longer color-only.** Three identical `●` differing only by
  hue said nothing under `NO_COLOR`, on a mono terminal, or to the ~8% of users
  who can't separate green from amber. The shape now carries it too: `●` awake,
  `◐` degraded, `○` unknown.

Verified by rendering under tmux at 110/120/80 columns and with `NO_COLOR=1`.
