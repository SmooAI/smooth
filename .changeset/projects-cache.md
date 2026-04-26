---
"@smooai/smooth": patch
---

Pre-open every registered project's `PearlStore` at Big Smooth startup and reuse those handles in `/api/projects` and `/api/projects/pearls`. Calling `PearlStore::open` from inside a tokio handler reliably wedges the spawned `smooth-dolt` Go subprocess in `pthread_cond_wait` and never returns (observed on smoo-hub: 60s+ timeouts on `/api/projects` while the same operation from a TTY returned in 50ms; `state.pearl_store.stats()`, which uses a store opened at startup, worked fine in the same process). Pre-caching at startup avoids the bad code path entirely. Trade-off: project registry changes need a service restart to populate.
