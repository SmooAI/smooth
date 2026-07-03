---
'@smooai/smooth': patch
---

docs: rewrite the Architecture vault for the post-teardown current state. Replaces the two-mode microVM framing (Direct-Mode / Sandboxed-Mode / Transport pages, now deleted) with a coherent set describing direct in-process dispatch (operative as host subprocess + `PermissionHook` role gating + `NarcHook` surveillance), a current-state Security-Model, an Extension-System (SEP) page marked to its real phase status (Phase 0 merged in the engine repo, Phase 1 landing), and a Daemon-Direction page for the `th-c89c2a` epic. Every claim verified against code (crate names, env vars, defaults, dispatch symbols); aspirational work labeled with pearl ids (auto-mode `th-515a13`, kernel sandbox `th-c89c2a`, SEP `th-2def2a`). Repoints stale ADR-001/002/003 links and two broken README source links to their real homes.
