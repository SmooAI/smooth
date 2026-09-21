---
'@smooai/smooth': minor
---

SmoothFlow: approving from SmoothFlow now works for aider, goose, crush and cline (th-5a2314). A scraped approval prompt used to be answered with Claude Code's `1`, which none of them read. Manifests now declare `[steer] approve_keys` / `allow_session_keys` / `deny_keys`. The defaults are Claude's `1` / `2` / `Escape`. The four built-ins set their own keys: aider `y`+Enter, goose Enter, crush Enter, cline `y`. The deny keys are the ones proven live. The conformance fake reads the raw bytes of the manifest's approve keys, and the rig requires them byte for byte, so a regression to a hardcoded key fails `permission`. `th harness doctor` now knows the four scraped harnesses: install commands, plus provider checks (goose `GOOSE_PROVIDER`, crush `providers`, cline `providers.json`, and aider's API key as a warning).
