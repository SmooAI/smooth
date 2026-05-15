---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-3f. New
`smooth_bigsmooth::tonic_clients` module providing UDS-dialing
client adapters for the in-VM cast. Method signatures mirror
the legacy HTTP clients so runner call sites can swap them in
without a rewrite:

* `NarcGrpcUds::judge(&JudgeRequest) -> JudgeDecision` — drop-in
  for `smooth_wonk::NarcClient`. Folds any transport / proto
  error into `EscalateToHuman` so Wonk fails closed.
* `ScribeGrpcUds::append(pb::LogEntry) -> bool` — replaces the
  HTTP Archivist forwarder with a client-streaming gRPC Log
  RPC. Entries are queued through a bounded mpsc and the
  background task owns the stream.
* `BigSmoothGrpcUds` — wraps the generated `BigSmoothClient` so
  callers can dial a UDS path instead of a hostname; exposed
  via `.client()` since the AccessStore RPC surface is large
  enough that callers want the full generated client.
* `GrpcCastClients::connect_all(socket_dir)` — convenience
  bundle resolving the three sockets against the standard
  `single_process::bootstrap_grpc_cast` layout.

Wiring into the operator-runner is deliberately deferred — the
adapters land first so iter-3g's smoke test can exercise them
end-to-end against `bootstrap_grpc_cast`. Phase 2 will replace
the runner's own cast-spawn path with these adapters when the
runner is co-resident with BS in the single VM.

5 new tests: Narc round-trip approves a safe domain over UDS,
Narc folds a dead socket to EscalateToHuman with the expected
reason, Scribe streams 3 entries that land in the gRPC-backed
MemoryLogStore, BigSmooth lists a freshly-filed pending
request, and `connect_all` resolves the standard socket
layout.

Also: moved `hyper-util` from dev-deps to deps on the
bigsmooth crate so the UDS connector code compiles outside
the test cfg.
