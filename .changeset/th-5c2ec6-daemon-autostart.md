---
'@smooai/smooth': patch
---

Desktop app reliably auto-starts its own daemon + open-at-login (th-5c2ec6, th-ccf2cf).

A stale saved `remoteUrl` used to make `startDaemon()` early-return in remote
mode, leaving a Mac launched via Finder/`open` with NO local daemon (phone
offline). Remote is now only the WINDOW's view target: the app always starts
this Mac's own local daemon in the background, surfaces the current mode in the
tray header + title, and logs the daemon's stdout/stderr and spawn errors to
`~/.smooth/desktop.log` (previously `inherit`ed and lost under `open`).

Also adds a first-run "Open at Login" default (`app.setLoginItemSettings`,
macOS `SMAppService`), user-toggleable from a tray checkbox, so the daemon
auto-starts on login and survives reboot without a hand-made launchd plist.
