---
'smooai-smooth-tools': patch
---

Phase 3 (EPIC th-c89c2a): route sandboxed shell egress through the goalie
proxy and deny direct bypass. `SandboxPolicy::with_proxy(host:port)` makes
the sandbox the egress boundary — it sets `HTTP(S)_PROXY`/`ALL_PROXY` (with
`NO_PROXY` for loopback) on the child, and the macOS Seatbelt profile
**denies direct `network-outbound`** except to loopback (the proxy + local
dev servers). A tool that ignores the proxy vars simply can't connect off-box.
Off-box traffic must therefore pass the proxy's exact-host allowlist. Without
a proxy configured, network is unrestricted as before (opt-in, no regression).
Verified live that the deny actually blocks external egress (direct curl →
connection refused) while the SBPL parses and benign commands run.
