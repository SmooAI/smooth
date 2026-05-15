---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-3d. Production wiring of the
`BigSmooth` gRPC `Orchestrator` trait via the new
`OrchestratorAdapter` over the existing `AccessStore`.

Fully wired RPCs (same semantics as the `/api/access/*` HTTP
routes):

* `FilePendingAccess` — files into the AccessStore, returns
  the freshly-stamped id + timestamp. Surfaces invalid
  `JudgeKind` as an empty id (the trait signature is
  infallible by design — clients detect via the empty id).
* `ResolveAccess` — drives `AccessStore::resolve`, mapping
  proto Verdict/Scope into the domain enums and surfacing
  `NotFound` / `InvalidArgument` via `tonic::Status`.
* `ListPendingAccess` — snapshot of currently-pending
  requests as proto `PendingAccess` messages.
* `SubscribeAccessEvents` — server-streams every
  Pending/Resolved/Expired event from the AccessStore's
  broadcast channel; recovers cleanly from `Lagged` and ends
  on client cancel.

Stubbed RPCs (land in Phase 2 / pearl th-ea2aa5 once `th up`
exists):

* `Dispatch`, `Cancel` — return `Unimplemented` with a clear
  pointer to the pearl.
* `ListOperators` — returns an empty list (bench harness
  probe needs this to not error).
* `SubscribeOperatorEvents` — returns immediately, ending the
  stream gracefully.

11 new tests in `smooth-bigsmooth` cover end-to-end round trips
over UDS for file/resolve/list/subscribe, plus the kind +
scope round-trip helpers, the unspecified-kind error path, the
not-found resolve path, and the dispatch/list-operators stubs.
