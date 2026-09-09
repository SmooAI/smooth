---
'@smooai/smooth': patch
---

SmoothFlow macOS: the unit-test host is inert (th-dccc80). `SmoothFlowTests` runs inside SmoothFlow.app, and every `xcodebuild test` used to start the real fleet against the developer's HOME — a `tmux -L smoothflow kill-server`, a stray `smooth-daemon` child, and before #546 a rewritten `~/.smooth/daemon.addr`. Under XCTest the app now builds its menu and stops (opt in with `SMOOTHFLOW_TEST_START=1`), `shutdown()` is a no-op before `start()`, and the scheme pins the host's HOME to `/tmp/smoothflow-xctest-home`.
