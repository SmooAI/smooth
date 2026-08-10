---
'@smooai/smooth': minor
---

Family AI M3 Phase B.2: route daemon `remember`/`recall` to cloud memory (th-5189f0)

Big Smooth can now sync agent memory to the platform memory home built in Phase
B.1 (org-scoped, owner-tiered `memories` REST on `api.smoo.ai`), so a family
member's `remember` is durable across every device the family signs in on.

- New `CloudMemory` (`smooth-daemon/src/cloud_memory.rs`) implements the engine's
  `Memory` trait against the B.1 REST (`POST /organizations/:org/memories`,
  `POST …/memories/search`, `DELETE …/memories/:id`), reading the live user
  bearer at call-time (the credential heartbeat keeps it fresh) — no re-implemented
  token refresh.
- **Fail-soft**: any cloud error (network, 5xx, missing/expired session) falls
  back to the daemon's local sqlite store, so a flaky platform never hard-fails a
  turn. Every fallback is logged.
- The `StorageAdapter::memory_for_access` seam now returns a `CloudMemory` bound to
  the turn's org + acting principal (local store as fallback) when cloud routing is
  on, else the local store unchanged.
- **Opt-in, default OFF** behind `SMOOTH_CLOUD_MEMORY` (`1`/`true`/`yes`/`on`) —
  zero behavior change until enabled. Enabling also requires the B.1 deploy; the
  entitlement/subscription gate is a separate stream (th-74e0f8).
- **Scope**: writes default to the **personal** tier (owner = the acting
  principal); an entry can opt into the org-wide **shared** tier via a
  `scope = "shared"` key in its memory metadata — the trait-preserving path for a
  future `remember({scope})` tool arg.
