---
"@smooai/smooth": patch
---

smooth-agent plugin 0.3.0: every session is now registered on the th-mail bus, not just `th claude run` workers.

The SessionStart hook previously no-op'd unless `SMOOTH_AGENT_HANDLE` was set (i.e. a Big Smooth worker). It now registers plain `claude` sessions too, under a stable per-(user, host, repo) handle (`<user>@<host>/<repo>`), so any session with the plugin enabled is active and mailable by Big Smooth and other agents. Workers still register under their `SMOOTH_AGENT_HANDLE`; a `SMOOTH_AGENT` override wins if set. `th` absent → the hook remains a no-op and the rest of the plugin (/smooth, skills, guardrails) still works.
