---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-3e. New
`smooth_bigsmooth::single_process` module brings the four cast
gRPC servers (Narc/Wonk/Scribe/BigSmooth) up on UDS sockets in
one shot when `SMOOTH_SINGLE_PROCESS=1` is set.

Socket layout under `socket_dir()`:

* `$SMOOTH_SINGLE_PROCESS_SOCKET_DIR/{narc,wonk,scribe,bigsmooth}.sock` — explicit override (tests).
* `$XDG_RUNTIME_DIR/smooth/` — XDG-compliant default.
* `/tmp/smooth-<pid>/` — last-resort fallback.

`bootstrap_grpc_cast` returns a `GrpcCastHandles` owning the
four `JoinHandle`s + socket paths + the fresh `MemoryLogStore`
the Scribe gRPC writes into. `shutdown()` aborts the tasks
and removes the socket files.

`bootstrap_from_app_state` is the BS-specific helper that
pulls `BoardroomNarc` + `AccessStore` straight from the
existing `AppState` and seeds a fresh Wonk `AppState` with a
permissive default policy (mirrors the legacy boardroom
spawn). The boardroom binary now invokes this after
`AppState::new` so the gRPC cast comes up co-resident with
the legacy HTTP cast — iter-3f will rewire the operator-runner
to dial the UDS sockets instead.

4 new tests: env-var contract, all-four-sockets-exist after
bootstrap, end-to-end gRPC round-trip per socket (Narc
GetCacheStats / Wonk PolicySummary / Scribe GetStats /
BigSmooth ListPendingAccess including a freshly-filed
request), and shutdown removes the socket files.
