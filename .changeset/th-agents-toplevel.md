---
'@smooai/smooth': patch
---

`th agents` is now a top-level command — the Smoo AI platform agent surface,
promoted alongside `th crm` / `th config` / `th testing`. `th api agents` still
works and always will: `th api` is the thin, route-shaped passthrough, while
`th agents` is the daily surface. Same code path, not a wrapper.

This required taking the `agents` spelling back from `th agent` (the machine-local
messaging registry), which carried it as a `visible_alias`. That alias is the
exact pairing `.claude/skills/normalize` rule 4 forbids by name — it made the
plural mean messaging — so it is removed rather than kept. `th agent` (singular)
is unchanged; only the plural moves.

Also fixes a dangerous annotation in `th agents tools list`. The unregistered
warning read "binds to nothing at runtime", an assertion whose documented
response is deletion. The registry route enumerates the TypeScript registry only,
so every Rust-side tool arrives flagged — in production that was 5 of Smantha's
11, including `verify_identity` while it was actively serving OTP. Following the
warning would have disabled auth and re-caused the incident this epic closed. The
warning now names both causes and says to verify. The underlying wrong data is
server-side and tracked in th-fddcc2.
