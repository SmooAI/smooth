---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-1 (spike). Wires tonic + prost +
tonic-build into the workspace and proves the gRPC machinery
works end-to-end with the smallest possible slice: the Narc
service compiled from `proto/narc.proto`, served over a UDS, and
exercised by a tokio test that round-trips Judge calls.

The smooth-narc crate now exposes `pb` (generated proto types),
`convert` (TryFrom/From between proto types and the existing
in-crate `judge::*` types), and `grpc` (a tonic server adapter
that wraps a `Judge` trait — implemented by the test stub here;
production impl in smooth-bigsmooth's BoardroomNarc lands in
iter-2). 13 new tests across conversions + UDS round-trips.

Iter-2 picks up the rest of Phase 1: wonk + scribe + bigsmooth
proto servers, then operator-runner client switch, then the
SMOOTH_SINGLE_PROCESS feature flag.
