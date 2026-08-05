---
'@smooai/smooth': patch
---

smooth-agent: nudge `git push` toward `scripts/ci/attest.sh` in repos that have it

New `attest-push-hint.sh` PreToolUse hook. Fires only where `scripts/ci/attest.sh`
exists, so it turns itself on in any repo adopting the convention and stays silent
everywhere else. Exits 1 (ask), not 2 (block) — attesting is often the wrong call,
and a red local check is sometimes the machine rather than the code.
