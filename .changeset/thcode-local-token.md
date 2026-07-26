---
'@smooai/smooth': patch
---

Fix `th code`: send the local auth token so it can actually connect
(pearl th-6dd202).

`th code` called `connect_async` with no authentication at all, while the
daemon runs the operator's **strict-auth** local flavor — so every session
died on `401 Unauthorized` even with a healthy Big Smooth, and the error
unhelpfully suggested `Run: th up` for a server that was already up.

Verified against the live daemon: no-auth → 401, `Authorization: Bearer` →
401 (the upgrade path doesn't consult headers), `?token=<correct>` → 101
Switching Protocols, `?token=WRONG` → 401. The client now resolves the token
the same way the daemon does (`SMOOTH_LOCAL_TOKEN` → `~/.smooth/operator-token`,
read-only — the daemon owns provisioning) and passes it as the query param,
percent-encoded so a token containing `&`/`#`/space can't silently truncate
the URL. A 401 now explains the token mismatch instead of misdirecting.
