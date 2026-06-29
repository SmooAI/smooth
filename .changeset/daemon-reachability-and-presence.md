---
'smooai-smooth-daemon': minor
'smooai-smooth-web': minor
---

EPIC th-c89c2a: make the daemon trivially reachable and give it a face + a voice.

- **Same-origin serve (th-a28904):** the daemon serves the smooth-web SPA at its
  own origin via a new `LocalServer.serve_spa()` seam, injecting the auth token
  into `index.html` (`window.__SMOOTH_TOKEN__`, read first by `operator.ts`), so
  `http://127.0.0.1:8787/` works with no `?api`/`?token` query string.
- **Tailscale auto-serve (th-ce286d):** on startup, when Tailscale is present and
  the node is up, the daemon exposes itself over the *tailnet* via `tailscale
  serve` (never `funnel` — tailnet-private), with a `SMOOTH_TAILSCALE_SERVE=0`
  opt-out and shutdown teardown.
- **Big Smooth persona (th-5f059b):** the daemon installs a personal-assistant
  system prompt via the operator's new `.persona()` seam, replacing the stock
  customer-support prompt.
- **smooth-web Presence glow-up (th-f1a1f0, th-833b5f):** the reactive Big Smooth
  face is now the room — a haloed greeting on the empty state, a sticky presence
  bar in conversation, the approval HITL emanating from him in amber, the
  Bricolage Grotesque wordmark, a green liveness dot, and parked tools reading
  "awaiting your okay".
