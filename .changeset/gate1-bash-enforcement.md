---
'smooai-smooth-tools': patch
---

EPIC th-c89c2a (th-515a13): Gate 1 deny is now enforced at the bash tool boundary.
A new `permission` module loads deny/ask/allow rules from `~/.smooth/permissions.toml`
(override `SMOOTH_PERMISSIONS_FILE`, process-global, best-effort) and the `bash`
tool now blocks any command its **deny** rules match — compound-split, so
`ls && rm -rf ~` is caught on the `rm`. A configurable complement to the hardcoded
circuit-breaker; an unconfigured daemon is unchanged (empty rules never deny).
Per-call `Ask`→HITL needs an operator host-hook seam (filed separately); `Ask`/
`Allow` proceed for now, with the name-based operator HITL covering confirmation.
