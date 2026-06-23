---
'smooai-smooth-goalie': patch
---

Phase 3 (EPIC th-c89c2a, P0 #3): add the egress boundary's security-critical
core — an in-process exact-host allowlist (`EgressAllowlist`) with a single
strict hostname parser (`normalize_hostname`). The parser rejects, before any
membership check, the bypass primitives that defeat host allowlists: embedded
NUL / non-ASCII labels (the `attacker.com\0.google.com` SOCKS5 class,
CVE-2025-55284), ports/schemes/userinfo/paths, and malformed DNS label
structure. The allowlist holds exact hosts only — wildcard/port entries are
dropped at construction (and returned for logging), so a bad config can only
narrow reachability, never widen it. The query host is normalized through the
*same* parser so a normalization mismatch can't sneak past. Pure, in-process
(no Wonk round-trip); the daemon's egress proxy + sandbox wiring follow.
Seven adversarial unit tests.
