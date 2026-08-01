---
'@smooai/smooth': patch
---

Big Smooth.app: auto-enable the menu bar on app launch + a local install script

The menu bar was env-only (`SMOOTH_MENUBAR=1`), so a double-clicked `Big Smooth.app` showed nothing. Now `smooth_menubar::enabled()` also returns true when the daemon is launched from inside a `.app` bundle (its executable path is `…/Big Smooth.app/Contents/MacOS/smooth-daemon`) — double-click / `open` / a login-item all light up the menu bar. A plain `smooth-daemon` on `$PATH` (CLI, tests, a bare launchd agent like smoo-hub's) stays headless.

Also: the menu bar now survives a server error (port busy, missing creds) instead of exiting — a menu-bar app must not silently vanish. And `scripts/macos/install-local.sh` builds + packages + installs `Big Smooth.app` to `~/Applications` on the local Mac (the laptop counterpart to `scripts/smoo-hub/deploy.sh`), with `--login-item` for auto-start and `--open` to launch it.
