---
'@smooai/smooth': patch
---

Fix `th pearls push` failing with `fatal: remote '' not found` (pearl th-2681fd). Newer Dolt reports a missing branch upstream with this string — the branch's upstream remote resolves to `''` on a bare `CALL DOLT_PUSH()` — but the CLI's first-push auto-retry predicate only matched the older "no upstream branch" wording, so the recoverable case surfaced as a raw error instead of retrying with `-u origin main`. The empty-remote form now triggers the set-upstream retry; a named missing remote (`remote not found: origin`) still classifies as "no remote at all" (global-store skip), and genuine divergence matches neither.
