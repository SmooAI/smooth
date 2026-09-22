---
'@smooai/smooth': patch
---

The `attest-push-hint` hook parses under macOS's /bin/bash 3.2 again. Bash 3.2 cannot parse a `case` pattern's closing `)` inside `$(…)`, so the hook died with a syntax error and blocked every agent's `git push`. The pattern now uses the leading-paren form `(pat)`, which both 3.2 and 5.x accept.
