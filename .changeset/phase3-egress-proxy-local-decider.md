---
'smooai-smooth-goalie': patch
---

Phase 3 (EPIC th-c89c2a): wire the `EgressAllowlist` into goalie's forward
proxy via a `NetworkDecider` (Wonk **or** in-process `Local` allowlist). The
always-on daemon can now run the proxy with `run_proxy_local(addr, allowlist,
audit)` — exact-host egress decisions with no Wonk network round-trip, fail-
closed by construction. The accept loop is factored into `serve()` and the
HTTP + CONNECT paths share one decision call; audit reasons now reflect the
actual decider. Backward-compatible: `run_proxy(addr, wonk, audit)` is
unchanged. Adds an end-to-end test (a real client through the proxy gets 403
for an unlisted host, contacting no upstream) plus a decider unit test.
