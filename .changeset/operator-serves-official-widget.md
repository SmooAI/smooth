---
'smooai-smooth-daemon': minor
---

EPIC th-c89c2a: `th daemon operator` now serves the official
`@smooai/smooth-operator` widget at `/`. `serve_local_flavor` enables the
operator's `serve_widget` seam with the local token, so the browser loads the
widget host page (token injected same-origin) and connects to the operator's own
`/ws?token=…`. One process, one port: the canonical WS protocol **and** a usable
chat UI. Additive — the bespoke control surface stays until the widget reaches
parity (markdown / tool-confirm / session list, tracked upstream).
