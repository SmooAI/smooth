---
"@smooai/smooth": patch
---

Fix release tooling: `sync-versions.mjs` step 3 no longer pins the external `smooth-operator-core` git dependency to the workspace version.

Step 2 already skipped `smooai-smooth-operator-core` (a git dep at a fixed rev providing its own 0.15.0), but step 3 — which adds a `version` key to deps that lack one — did not, so it spliced `version = "0.19.0"` onto it and `cargo` failed to resolve (`requirement ^0.19.0 … candidate 0.15.0`). That blocked the 0.19.0 release once `smooth-scribe` consumed the engine. Step 3 now skips it too.
