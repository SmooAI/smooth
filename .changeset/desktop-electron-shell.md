---
'@smooai/smooth': patch
---

Add the Big Smooth desktop shell (`desktop/`, pearl th-a59af5): an Electron app that manages the native `smooth-daemon`, opens a window on the daemon's web UI, and provides a cross-platform `th` tray with Open / Set Up / Quit. It attaches to an already-running daemon and only terminates one it spawned itself. Packaging and signing are deferred — `electron-builder.yml` is a skeleton.
