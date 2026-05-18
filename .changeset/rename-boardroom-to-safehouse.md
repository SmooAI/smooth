---
'@smooai/smooth-cli': minor
---

Rename "The Boardroom" to "The Safehouse" everywhere.

Pre-[[ADR-001]] there were multiple microVMs and "Boardroom" named the one Big Smooth + the cast lived in. After consolidation there's just one VM, and the corporate-coded name jarred against the rest of the heist/mob naming family (Big Smooth, Narc, Bootstrap Bill, Wonk, Goalie, Scribe, Smooth Operators). The Safehouse fits the metaphor: a sealed place the family runs jobs from. See `docs/Decisions/ADR-003-rename-boardroom-to-safehouse.md`.

Code identifiers, env vars (`SMOOTH_SAFEHOUSE_MODE` / `_PORT` / `_IMAGE`), file names, tracing fields, OCI image (`ghcr.io/smooai/safehouse:latest` with entrypoint `/opt/smooth/bin/safehouse`), and docs all flip. No backwards-compat fallbacks — this is dev tooling, not a release artifact.
