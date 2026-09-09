---
'@smooai/smooth': patch
---

SmoothFlow macOS: quit is honored from every sender — ⌘Q, the menus, AppleScript `quit`, `NSRunningApplication.terminate()` — and is bounded. The app used to block in `waitUntilExit()` on its child daemon inside `applicationWillTerminate`, so a daemon that did not exit on SIGTERM left the app running forever after the Quit Apple event was accepted (the 0.2.0 "quit did nothing, had to kill the pid" symptom). `applicationShouldTerminate` now takes the fleet down first and always answers `.terminateNow`; the `sh` supervisor records the daemon pid and escalates TERM→KILL after 3 s, and the app SIGKILLs the supervisor + daemon after 5 s as a backstop. Pinned by supervisor XCTests and a `QuitUITests` lane with a `trap '' TERM` daemon (th-6198bf).
