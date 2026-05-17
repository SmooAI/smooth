---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 4 iter-6c. Finishes the runner-side
gRPC Narc wiring iter-3f left as a TODO. Wonk's escalation
slot now accepts either HTTP or UDS transport, and the
operator-runner picks the right one at startup.

Shape of the change:

* New `smooth_wonk::NarcEscalator` trait —
  `async fn judge(&self, request: &JudgeRequest) -> JudgeDecision`.
  Implementors must fail closed on any transport error so the
  contract matches the legacy HTTP client.
* The legacy `NarcClient` (HTTP) impls the trait.
* New `smooth_wonk::NarcGrpcUds` — UDS-dialing gRPC client
  implementing the same trait. Moved from
  `smooth_bigsmooth::tonic_clients::NarcGrpcUds` so wonk is
  the canonical home (it's the crate that needs to USE a
  Narc client). `smooth_bigsmooth::tonic_clients::NarcGrpcUds`
  is now a re-export for back-compat with iter-3f imports.
* `AppState::with_narc` now takes any `NarcEscalator` impl —
  HTTP `NarcClient`, the new UDS client, or a test stub. The
  internal field is `Option<Arc<dyn NarcEscalator>>`. A new
  `with_narc_arc` accepts a pre-Arc'd value for callers
  hot-swapping clients.
* operator-runner's `spawn_cast` now branches on
  `SMOOTH_SINGLE_PROCESS=1`: when set, dial Narc via UDS at
  `$XDG_RUNTIME_DIR/smooth/narc.sock` (override via
  `SMOOTH_SINGLE_PROCESS_SOCKET_DIR`). Else keeps the legacy
  `SMOOTH_NARC_URL` HTTP path. UDS connect failure logs and
  proceeds with no arbiter (Wonk hard-denies non-allowlisted
  requests, same fail-closed shape).
* `Cargo.toml`: `tower` + `hyper-util` move from wonk
  dev-deps to deps so the UDS client compiles outside test
  cfg.

3 new tests in `smooth-wonk::narc_grpc_uds`: round-trip
approve over UDS via a stub Judge server; dead-socket-after-
connect folds to EscalateToHuman; missing-socket connect
errors with a clear message. The two equivalent tests in
`smooth-bigsmooth::tonic_clients` are dropped (they
exercised the same paths from a duplicate impl).

75 wonk tests pass; 269 bigsmooth lib tests pass; iter-3g
smoke test passes after a single `use smooth_wonk::NarcEscalator`
import (the trait must be in scope to call `.judge` through
the trait object).
