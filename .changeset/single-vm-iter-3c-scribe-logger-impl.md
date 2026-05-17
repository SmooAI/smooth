---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-3c. Production wiring:
`GrpcLogStoreAdapter` implements `smooth_scribe::grpc::Logger`
on top of any `LogStore`, including the existing
`MemoryLogStore`. The proto Scribe surface (Log /
Query / GetStats) now drives the same in-memory ring the legacy
`/log` HTTP endpoint feeds.

The domain `LogEntry` predates the proto contract by a wide
margin, so this module owns the proto<->domain conversion.
Lossy in two well-defined ways:

* `pb::Level::Trace` and `pb::Level::Unspecified` fold to
  domain `Debug` / `Info` (domain has no Trace).
* Domain `id` (uuid) has no proto equivalent — generated on
  append, dropped on emit. Queries match on the rest.

The proto QueryRequest is richer than the in-store `Query`
(since/until/operator_id/bead_id/trace_id/message_contains
on top of source/min_level/limit). The cheap subset is pushed
to the store; the rest is applied in-process during the walk.

9 new tests in `smooth-scribe` cover level/entry round-trips,
client-streaming append, server-streaming query with source +
min-level + case-insensitive message filters, GetStats's
total_entries counter, and the `adapter_for_memory_store()`
convenience.

Tech-debt: forwarder.rs + hook.rs + log_entry.rs + server.rs +
store.rs have narrow `#![allow(clippy::expect_used)]` annotations
for pre-existing `.expect()` calls so iter-3c's quality gate
runs cleanly. The forwarder + HTTP server retire in Phase 4
once the gRPC Scribe is the only ingest path; cleanup happens
then rather than in this iter.
