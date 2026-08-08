---
'@smooai/smooth': patch
---

attest-push-hint now BLOCKS a bare `git push` (exit 2) instead of asking (exit 1).

Measured on 2026-08-08: of 18 Claude transcripts that ran a `git push` that day, 7
had the hook fire and zero ran `attest.sh` or used the `attest:ack` bypass. The last
15 merged smooai PRs carried no `ci-attest` statuses and paid full CI. An ask is
something an agent in auto mode approves for itself, so the nudge was a log line
with extra steps.

Declining is unchanged and still cheap — append ` # attest:ack reason=...` — it just
has to be explicit now. Adds a committed 17-case test suite; this hook had shipped
broken twice with no test to catch it.
