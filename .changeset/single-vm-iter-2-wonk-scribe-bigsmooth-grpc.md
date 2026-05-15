---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-2. Applies iter-1's tonic-over-UDS
pattern to the three remaining cast crates: Wonk, Scribe, and
Big Smooth. Each gets a `pb` module (tonic-generated types), a
`grpc` module (server adapter wrapping a small per-service
trait), and `serve_uds` for spawning the server on a Unix socket.

- **smooth-wonk** — Wonk service over UDS. `Checker` trait
  abstracts CheckNetwork/Tool/Cli/File + ReloadPolicy/Summary.
  Verdicts carry `was_escalated` + `resolved_scope` so callers
  can distinguish policy-decided from human-resolved approvals.
  Wonk's proto imports narc.proto for `Scope`; tonic-build
  routes through smooth-narc's existing `pb` module via
  `extern_path`.
- **smooth-scribe** — client-streaming Log + server-streaming
  Query. `Logger` trait abstracts append/query/stats. mpsc
  channel back-pressures the store walker on slow consumers.
- **smooth-bigsmooth** — Dispatch + Cancel + AccessStore
  CRUD + AccessEvents/OperatorEvents server-streams. The
  `Orchestrator` trait is wide (10 methods) but each method
  maps 1:1 to a proto RPC. Production wiring (into the
  existing AppState + AccessStore) lands in iter-3.

Proto-include change: both wonk.proto and bigsmooth.proto now
import `"narc.proto"` (relative within the workspace proto/
root) instead of the full `"smooth/narc/v1/narc.proto"` package
path. Cleaner with our flat proto/ layout.

17 new tests across the three crates:
- wonk (5): network allowed/denied, tool round-trip, file
  Unspecified→InvalidArgument, resolved_scope flow-through.
- scribe (4): Log client-streaming, Query server-streaming,
  GetStats, back-pressure drop.
- bigsmooth (4): Dispatch, AccessStore CRUD round-trip,
  AccessEvents stream, OperatorEvents stream.

Iter-3 picks up: the production trait impls (BoardroomNarc as
Judge, AppState as Wonk Checker + Scribe Logger + BigSmooth
Orchestrator), the operator-runner client switch, and the
SMOOTH_SINGLE_PROCESS feature flag that selects the new path.
