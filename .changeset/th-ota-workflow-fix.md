---
'@smooai/smooth': patch
---

Fix the Electron OTA publish workflow startup-failure (desktop-publish.yml).

The publish steps gated on `secrets.OTA_PUBLISH_ROLE_ARN != ''` inside a step-level `if:` — but the `secrets` context isn't available there, which is a **startup-failure**: GitHub refused to start the whole workflow on every push (and would on a tag/dispatch too), so the OTA pipeline had never actually run and no update was ever published. The secret is now hoisted to a job-level `env` (`OTA_PUBLISH_ROLE_ARN` + a `HAS_OTA` presence flag) and the OTA steps gate on `env.HAS_OTA == 'true'`, preserving the intended graceful-degrade (skip publish when unset). Unblocks Big Smooth desktop OTA once the downloads.smoo.ai stack is deployed and the CI secrets/vars are set.
