---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-3b. Production wiring: Wonk's
existing `AppState` implements `smooth_wonk::grpc::Checker`, so
the same policy + Narc-escalation logic that drives the
`/check/*` HTTP handlers now drives the gRPC `CheckNetwork`,
`CheckTool`, `CheckCli`, `CheckFile`, `ReloadPolicy`, and
`PolicySummary` RPCs. Iter-3e will spawn this in-process over a
UDS when `SMOOTH_SINGLE_PROCESS=1` is set.

Decision logic intentionally mirrors the HTTP handlers in
`server.rs` for this iter — the dedup happens in Phase 4
cleanup once the HTTP surface is retired. The Checker still
escalates to Narc via the existing HTTP `NarcClient` (option
(a) in the plan); iter-3f swaps that for the gRPC client.

10 new tests in `smooth-wonk` exercise the trait end-to-end:
static-allowlist approve, auto-approve-domain approve,
unknown-domain deny, tool allow/deny/unknown, file inside-mount
allow + outside-mount + traversal deny, dangerous-CLI flag, the
PolicySummary RPC, and a sanity check that the trait routes
into AppState's policy holder.
