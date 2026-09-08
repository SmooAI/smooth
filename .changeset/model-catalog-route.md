---
'@smooai/smooth': patch
---

th-1d8007: the daemon serves the model catalog as a single source of truth.

The bench-scored model lineup was hand-copied across `smooth-web` (`model-scores.json` + `modes.ts`), iOS, Android, and the TUI, so it drifted (mobile showed the old Flash/Code/UI modes while desktop was bench-derived). The daemon now serves the canonical bench output (`docs/model-scores.json`) at `GET /api/model-catalog` — ungated public data. `smooth-web`'s picker fetches it on open (`fetchModelRows`) and derives its rows from the live data, falling back to the bundled copy when the daemon is unreachable. A bench refresh now reaches every client that fetches this route without a per-platform re-copy. Mobile clients consuming the route land separately.
