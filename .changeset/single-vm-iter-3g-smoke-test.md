---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-3g. End-to-end smoke test for the
single-VM gRPC cast — confirms iter-3a..3f wire together as a
system. Lives in `crates/smooth-bigsmooth/tests/` so it doesn't
share a socket-dir namespace with the parallel unit tests.

Coverage:

* `single_process_cast_round_trips_a_narc_then_resolve_flow` —
  bootstrap → connect_all → Narc.judge auto-approves a known
  safe domain → BigSmooth.file_pending_access seeds the store
  → list shows the pending entry → Scribe streams five entries
  that land in the gRPC-backed MemoryLogStore → AccessStore
  resolution clears the pending list. All five RPCs cross UDS.
* `bootstrap_shutdown_rebootstrap_cycle_works` — exercises the
  shutdown path's socket-unlink contract by re-bootstrapping
  against the same directory.

Closes the gRPC-collapse arc for Phase 1: each cast member has
its wire surface (iter-2), each is production-wired (iter-3a..d),
BS spawns them on UDS under the flag (iter-3e), client adapters
exist for the runner (iter-3f), and the smoke confirms it
holds together (iter-3g). Phase 2 (pearl th-ea2aa5) flips the
sandbox topology to put the runner in the same VM as BS so it
actually dials these sockets.
