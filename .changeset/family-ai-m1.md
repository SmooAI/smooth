---
'@smooai/smooth': minor
---

Family AI M1+M2 (ADR-008, th-12d875): per-principal tool RBAC + Smoo Jr mode. Big Smooth can now be a family agent — each member authenticates with their own local token and the daemon narrows the per-turn tool set to that member's role (deny-by-default, enforced in the tool provider from the principal's `role:` group, zero engine changes). Smoo Jr is the hardened child role: allowlist-only tools (no shell/writes/egress/delegation), enforced from the connection's token (not the mode picker), and surfaced as a selectable `smoo-jr` mode. Config lives in `~/.smooth/family.toml`; absent/malformed is fail-closed (single-tenant, member tokens rejected). Deferred to later milestones: per-principal memory partition, org-scoped relay routing, per-role child-safety Narc, and the COPPA gate before any public under-13 account.
