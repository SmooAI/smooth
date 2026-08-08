---
'@smooai/smooth': patch
---

Fix the desktop-publish workflow failing to parse on every run. The two OTA publish steps gated on `secrets.OTA_PUBLISH_ROLE_ARN != ''`, but the `secrets` context is not available in a step `if:` — GitHub rejected the whole file with "This run likely failed because of a workflow file issue" before any job started, so the Electron/OTA packaging path had never actually run. The secret is now hoisted to job-level `env` and the conditions read `env.OTA_PUBLISH_ROLE_ARN`.
