---
'@smooai/smooth': minor
---

One `@`-mention backend for every Big Smooth face (pearl th-8e9cf6, epic th-d7366d). The daemon's `GET /search` now serves pearls (open + in-progress, from the workspace's `.smooth/dolt` with a 30s TTL cache) alongside files and paths — resolving the v1 deferral, so the web composer gets pearl mentions for free — and accepts a guarded `?cwd=` override so a client can search its own working directory (honored only for the daemon workspace or paths under the user's home). `th code`'s `@` picker now queries that same `/search` endpoint and overlays the daemon's ranked results when they arrive (generation-guarded against stale replies), keeping its local walk purely as the offline fallback.
