---
"@smooai/smooth": patch
---

`th pearls push/pull`: raise the Dolt remote-sync wallclock bound from 30s to 300s. The bound SIGKILLs any sync exceeding it and returns a retryable error, but each retry restarts the same transfer — so a legitimately large push (observed ~150s to re-upload a 303M pearl store's post-gc 114M oldgen table over a home uplink) could never complete at 30s. `DOLT_PUSH` is a single synchronous SQL call with no progress stream, so the upload is byte-silent and a stall detector is unworkable; the bound's only real job is preventing an *infinite* dead-socket wedge, which 300s still does. The `SMOOTH_DOLT_SYNC_TIMEOUT_SECS` override (and `0` = unbounded) is unchanged; normal incremental pushes finish in ~10s and are unaffected.
