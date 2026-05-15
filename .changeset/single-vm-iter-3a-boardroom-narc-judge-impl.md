---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 1 iter-3a. Production wiring: BoardroomNarc
implements smooth_narc::grpc::Judge so the existing decision flow
serves the new gRPC Narc surface unchanged. The trait's signature
already matched BoardroomNarc::judge — this is mostly the impl
declaration plus a `narc_grpc::serve_uds` wrapper for the BS
startup glue (iter-3e).

7 new tests in smooth-bigsmooth — drive the real BoardroomNarc
over UDS gRPC end-to-end: rule-engine approve for npmjs.org,
rule-engine deny for pastebin.com, EscalateToHuman for unknown
domains without an LLM, persistent-grant short-circuit, cache-len
round trip through GetCacheStats, sanity check that the trait
routes to the inherent method.
