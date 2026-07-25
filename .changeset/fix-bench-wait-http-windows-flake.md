---
"@smooai/smooth": patch
---

Fix a Windows-only flake in the smooth-bench `wait_for_http_accepts_any_http_status` test. The mock server was one-shot and replied without draining the request; on Windows the early close raced the client's request write (WSAECONNRESET) and the one-shot listener left no listener for the retry. It now loop-accepts and drains the request before replying.
