---
'@smooai/smooth': patch
---

smooth-agent: attest-push-hint missed `cd <repo> && git push` (the common shape)

Resolved the repo from the session's `.cwd`, which is never where the command
runs — agents `cd` inline. Also fixed push-detection so `git -C <path> push` is
recognised (the flag's value was eating the `push` token).
