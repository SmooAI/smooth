---
'@smooai/smooth': patch
---

th-e74aa6: direct-dispatch operative discovery now probes `$CARGO_TARGET_DIR/{release,debug}` (shared/isolated target dirs never land in `<repo>/target`), and the not-found error lists every probed location plus `pnpm install:th` as the blessed fix.
