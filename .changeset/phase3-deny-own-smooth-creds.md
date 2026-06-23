---
'smooai-smooth-tools': patch
---

Phase 3 hardening (th-08e05a / EPIC th-c89c2a): the macOS Seatbelt bash
sandbox now also kernel-denies reads of the daemon's *own* credentials in
`~/.smooth` — `providers.json` (the LLM API key) and the `auth/` directory
(the auth JWT). Previously a sandboxed shell tool could `cat
~/.smooth/providers.json` and exfil exactly the secret that drives the
agent. Project-scoped `<workspace>/.smooth` pearls stay readable (different
path). Adds an adversarial test that plants a sentinel under `~/.smooth/auth`
and proves the sandbox can't read it.
