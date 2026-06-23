---
'smooai-smooth-daemon': patch
---

Phase 3 (EPIC th-c89c2a): make the egress boundary adoptable out-of-the-box.
`SMOOTH_EGRESS_ALLOWLIST` now understands a `defaults` token that expands to a
curated set (`DEFAULT_EGRESS_HOSTS`) of the hosts an agent's shell legitimately
reaches — package registries (npm/yarn/crates/pypi), source hosts
(github/raw/codeload), and the Smoo platform — and merges with any of your own
exact hosts (`SMOOTH_EGRESS_ALLOWLIST="defaults, mycorp.internal"`). The
sentinel never surfaces as a rejected host. Verified live: with `defaults`,
github is reachable through the proxy (200) while an unlisted host is blocked
(403). Docs updated.
