---
'smooai-smooth-daemon': patch
'smooai-smooth-goalie': patch
---

Phase 3 (EPIC th-c89c2a): wire the egress boundary into the daemon
end-to-end. New `config::resolve_egress()` reads `SMOOTH_EGRESS_ALLOWLIST`
(comma/space-separated exact hosts; opt-in like auth/sandbox) and
`SMOOTH_EGRESS_PROXY_ADDR`. On startup the daemon spawns goalie's
`run_proxy_local` on a loopback port, threads the proxy address onto
`AppState`, and the runner registers tools via
`register_default_tools_with_proxy` — so agent `bash` egress is forced
through the proxy's allowlist (direct off-box network kernel-denied). goalie
re-exports `run_proxy_local`/`run_proxy_with`/`NetworkDecider`. Invalid
allowlist entries are dropped and logged. Verified live: with an allowlist
set, the daemon logs `egress boundary ON`, the proxy listens, and a request
to an unlisted host returns 403.
