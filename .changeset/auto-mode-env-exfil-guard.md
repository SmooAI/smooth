---
'@smooai/smooth': patch
---

**Security:** auto-mode no longer treats environment dumps as safe read-only commands. `env` and `printenv` were on the `SAFE_BASH_BINS` allowlist, so the chat agent auto-ran `env | sort` on a social-engineering prompt and posted the host environment (including `SSH_CONNECTION` topology) to an untrusted chat participant (pearl th-08f304). A new `dumps_environment` guard now denies environment-revealing commands as an exfiltration risk — `env`/`printenv` (dump forms only, not the `env FOO=bar cmd` setter), bare `export`/`set`/`declare -p`, `/proc/<pid>/environ` reads, and `echo`/`printf` of secret-named `$VAR` expansions — mirroring the existing credential-path deny. Wonk previously would have gated this; it was removed with the microVM stack (th-f4a801), and auto-mode (th-515a13) is its replacement.
