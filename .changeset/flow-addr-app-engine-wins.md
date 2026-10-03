---
'@smooai/smooth': patch
---

SmoothFlow: the app's own engine now owns `~/.smooth/flow.addr` even while Big Smooth is running. The app's engine keeps a separate session store, so a long-running Big Smooth that held the file sent harness hooks, `th flow`, the MCP flow tools and SmoothFlow Desktop to an engine that had none of the app's sessions. Big Smooth still never takes the file from a live holder. The daemon also shuts down cleanly on SIGTERM (how the app stops it), so it releases `flow.addr` instead of leaving a dead address that every client tried first (th-5069eb).
