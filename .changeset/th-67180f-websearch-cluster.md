---
'@smooai/smooth': patch
---

Big Smooth `web_search` prefers the Smoo cluster search over keyless public APIs (pearl th-67180f).

The daemon's `web_search` tool now tries `th search` (the org's own cluster search — better ranking, and the only source that synthesizes an answer) **first**, and falls back to the in-process keyless public APIs (`search_native`) only when the cluster is unavailable (`th` missing, not logged in, or api.smoo.ai unreachable). This inverts th-7031ba, which had made native the default and left the cluster reachable only on `answer: true` — so a logged-in daemon (e.g. smoo-hub) now gets real cluster results for every search, while a bare machine still degrades gracefully. Results come back as clean stdout (via a new `capture_th` helper) rather than the `$ th …` debug frame.
