---
'@smooai/smooth': patch
---

Desktop: don't run the PWA service worker in the Electron app — no more double update prompt, no stale SPA (th-003dc7).

The desktop app updates via its own OTA (electron-updater) and serves the SPA from the bundled daemon, so the service worker only produced a SECOND "A fresh Big Smooth is ready / Refresh now" prompt on top of the app's "Restart now", and a stale cached SPA after an OTA. Push on desktop is native (`window.bigSmooth.notify`), not the SW, so nothing there depends on it. `PWAUpdater` now detects the desktop shell (`window.bigSmooth`) and, instead of registering the SW updater, tears down any existing service worker + caches — the Electron webview always loads fresh from the local daemon. Browser and installed-PWA (mobile/web) users keep the full update-prompt flow unchanged.
