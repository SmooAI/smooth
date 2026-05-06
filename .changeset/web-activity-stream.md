---
"@smooai/smooth": patch
---

smooth-web: surface coding-workflow activity inline (parity with TUI's th-c83d13)

The `/ws` endpoint already broadcasts every `ServerEvent` — but the
chat page only filtered for `BigSmoothThought` to drive the floating
bubbles next to Big Smooth's face. Iteration boundaries, snapshot
saves, max-iter caps, budget breaches, and Narc warnings were
silently dropped on the web client (the TUI got them in pearl
`th-c83d13`).

Frontend-only change (backend already emits everything):

- `Msg` interface gains a third role `'activity'` for ephemeral
  status breadcrumbs. They live only in the live-session
  `messages` state — a page refresh drops them since
  `ChatMessageView` doesn't persist them.
- `chat.tsx`'s `/ws` `onmessage` handler now branches on:
  - `PhaseStart { iteration, alias }` → `→ iteration N • {alias}`
  - `CheckpointSaved { iteration }` → `✓ snapshot taken (iter N)`
  - `MaxIterationsReached { max }` → `⚠ hit max iterations (N) — stopping`
  - `BudgetExceeded { spent_usd, limit_usd }` → `⚠ budget exceeded — spent $X of $Y`
  - `NarcAlert` (`severity === 'Warn'` only) → `⚠ Narc Warn • {category}: {msg}`
- Activity messages gate on a `streamingRef` so a background
  dispatch for another session doesn't pollute the current view.
  `/ws` is broadcast across all sessions and `ServerEvent`s don't
  carry session ids today.
- The renderer treats `role: 'activity'` distinctly: thin
  monospaced one-line, muted by default, amber when the line
  starts with `⚠`. No avatar, no role label — they should feel
  like terminal status lines, not chat bubbles.
- Block-severity Narc alerts are unchanged; they still flow
  through the regular error path so we don't double-render.

Floating thought bubbles (the Fast-slot first-person summarizer)
are unaffected — additive, not in conflict.
