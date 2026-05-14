---
'@smooai/smooth': patch
---

creds: credential helper broker — Docker-spec stdin/stdout binary +
`/api/creds/issue` route. Sandbox tools that need authentication
(git clone over HTTPS, gh CLI) get short-lived credentials minted
by Big Smooth after a human approves the issue, instead of either
shipping a long-lived PAT into the VM or denying the call. v1
supports `github.com` via the host's `gh auth token`; AWS / Docker
Hub / generic username/password are separate pearls.

Flow:
- `smooth-credential-helper get` reads `{ServerURL: ...}` from stdin
- POSTs to `/api/creds/issue` on Big Smooth
- BS checks wonk-allow.toml first (fast path); else files an
  AccessStore Ask
- On approve at user/project scope, the host gets persisted to
  wonk-allow.toml so future mints skip the prompt
- BS mints by calling the host's `gh auth token` (resolved against
  the same richer PATH `host_tool` uses, so it works under launchd)
- Returns `{Username: "x-access-token", Secret: "ghs_..."}` to the
  helper, helper writes it back to git's credential framework

19 new tests: 9 unit (backend selection, host extraction, scope
serde, error display, mint error path), 4 helper bin (protocol
PascalCase, IssueBody omits-empty, NO_CREDS git-compat string),
6 integration (empty server → 400, pre-approved fast-path skips
pending, human approve → 200, human deny → 403, pick_backend
github subdomains, Ask shape carries kind=creds + full URL).

Pearl th-08b65f. Mounting the helper inside the sandbox image
(symlink at /usr/local/bin/git-credential-smooth, `git config
--global credential.helper smooth`) lands in a follow-up pearl —
the broker + binary protocol are the core that future scopes (AWS
STS, npm, Docker Hub) plug into.
