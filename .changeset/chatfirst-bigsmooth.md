---
"@smooai/smooth": minor
---

Big Smooth is now the chat-first daemon on the smooth-operator LocalServer engine
(th-7225f9 / epic th-c89c2a) — a showcase of `../smooth-operator`.

- **Chat-first UI**: `crates/smooth-web` swapped from the multi-page dashboard to
  the chat-first SPA (`App.tsx` + `BigSmoothFace`, same-origin token injection).
  `th daemon run` launches it; `th up` now boots the same chat-first daemon
  (via `SMOOTH_ADDR`) instead of the old in-process server.
- **Engine migration** `b43c04fe` → `d03fa10` (0.16.0): the whole workspace pins
  the core rev that `smooth-operator-server`/`svc` @ `487d10bc` are built on, so
  the daemon links the canonical LocalServer + SEP extension host (env-gated by
  `SMOOTH_EXTENSIONS_ALLOW`, discovered from `~/.smooth/extensions`, installed
  via `th ext`). All engine deps are git-revs (CI-buildable) — no path-deps.
- **New crates**: `smooth-daemon` (the daemon), `smooth-tools` (sandboxed
  fs/grep/bash tools), `smooth-goalie`.
- **Dropped old cast**: `smooth-bigsmooth`, `smooth-narc`, `smooth-scribe`,
  `smooth-archivist`, `smooth-operative`, `smooth-bench`, `smooth-tunnel` — the
  microVM-era Big Smooth and its old-signature engine hooks. `th bench` and
  `th tunnel` are removed with their backing crates.
- The narc LLM-judge and auto-mode permission cards (previously in the dropped
  crates / the `th code` TUI) re-home onto the new engine's `NarcHook` /
  `ToolHook` seam in a follow-up (th-3119e3). Every other `th` CLI feature
  (crm, agents, knowledge, crawl, search, files, widgets, booking, …) is
  unchanged.
