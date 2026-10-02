---
'@smooai/smooth': patch
---

SmoothFlow Desktop gets an end-to-end test against a real `smooth-daemon` (th-032792). The app's non-GPUI core (`app_core::Core`: engine events, New Session, attach/resize, keystrokes) moved into a library that the GPUI window wraps, so `apps/smoothflow-desktop/tests/e2e.rs` drives the same code the window runs. The test boots an isolated daemon, discovers it, creates a shell session through New Session, attaches at 80x24, types a command through the key encoder, and checks that the output reaches the terminal model. A second variant runs the daemon with a Finder PATH (th-9f6814). A third makes the launch fail and checks that the app sees a `dead` session with `launch_failed` and its detail. The pane now shows that reason instead of a blank terminal. CI runs the e2e on Linux and macOS, and daemon or flow-engine changes now trigger the desktop workflow.
