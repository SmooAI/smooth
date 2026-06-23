---
'smooai-smooth-tools': patch
---

Phase 3 hardening (th-08e05a / EPIC th-c89c2a): scrub secret-bearing
environment variables from sandboxed shell subprocesses. `env`/`printenv`
are read-only-classified (auto-allowed), and the bash tool inherited the
daemon's full environment — so a sandboxed `env` could dump the daemon's
own `SMOOTH_API_KEY` / `SMOOTH_DAEMON_TOKEN` and provider `*_API_KEY`s
straight into the transcript (the env-var twin of the `~/.smooth` read
hole). `SandboxedCommand::shell` now removes secret-named vars
(`SMOOTH_*`, `*_API_KEY`, `*_TOKEN`, `*_SECRET`, `*PASSWORD*`, `*_PAT`, …)
from the child env at the single spawn point — platform-independent, so it
also protects the not-yet-sandboxed Linux path. Benign vars (PATH, HOME, …)
are preserved. Adds unit + adversarial tests.
