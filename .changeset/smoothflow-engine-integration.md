---
'@smooai/smooth': patch
---

SmoothFlow macOS shell now runs against the real flow engine (pearl
th-f7f823): text WebSocket frames, the daemon's local token on every flow
route, ghostty actions raised off the main thread no longer trap, window
resizes reach the engine, surfaces re-attach after a reconnect or a
`--resume` relaunch, and done/dead rows are never attached. The child daemon
gets its own operator + flow stores, the app-owned `smoothflow` tmux socket
and no tailnet exposure, runs under a supervisor that dies with the app, and
`SMOOTHFLOW_DAEMON_BIN` / Settings point a dev build at any engine binary. New
**activity** tab (⌘⌥4) renders the engine follow-up's `flow.event` frames;
`flow.handoff` pushes the pearl rail; the client sends `flow.hello`. TCC probe
from a pane the real engine created: responsible = SmoothFlow, Calendar
granted.
