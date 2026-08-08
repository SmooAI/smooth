---
'@smooai/smooth': patch
---

Fix the `resolve_org` tests, which passed by ordering luck rather than by asserting anything (pearl th-4c6b2a).

`resolve_org` falls through to its in-memory credentials only when `active_org::resolve` finds nothing — and that reads process-global env **plus** the three credential stores under `~/.smooth/auth/`. The tests cleared `SMOOAI_ORG_ID` but never the stores, so on any machine with a real active org the store branch answered first. Run filtered they failed outright; in the full suite they passed only because an earlier test happened to clear the env, and any concurrent `th` invocation flipped them back.

They now run against a temp `HOME` with no credential stores, under a lock, with the previous environment restored. A new test asserts the fixture itself hides the real active org, so the isolation cannot silently stop working.

