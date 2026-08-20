---
'@smooai/smooth': patch
---

th-1d5ca8: `SMOOTH_MCP_ALLOW_WRITE=0` is a machine-level write kill switch for `th mcp serve` — every tool not annotated `read_only_hint` is hidden from the advertised roster and rejected if called anyway, failing closed on unannotated tools. Also annotates `pearls_ready` as read-only (the one genuine read that was missing the hint, which the fail-closed gate exposed).
