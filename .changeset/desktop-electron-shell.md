---
'@smooai/smooth': patch
---

Big Smooth desktop (`desktop/`, pearl th-a59af5): the Electron app is now the installable — it bundles `smooth-daemon` as its engine, packages via electron-builder (signed .dmg/.zip on macOS with the hardened runtime and the TCC usage strings; NSIS on Windows), and its tray drives Open / Set Up / Quit. Adds `smooth-daemon tcc calendar|reminders` for the grant flows, and `SMOOTH_MENUBAR` now turns the native menu bar OFF as well as on, so a bundled daemon doesn't raise a second status item. Calendar/Reminders prompts do not fire from a spawned child yet — measured, documented in `desktop/README.md`.
