---
'@smooai/smooth': patch
---

Fix the desktop "Enable notifications" button doing nothing. The Electron webview can't do Web Push (Chromium ships no push service, so `pushManager.subscribe()` rejects and the click silently no-ops). `usePush` now detects the Electron bridge and falls back to native OS notifications over a new preload IPC channel: Enable fires a confirmation notification, and finished replies are relayed natively when the window is unfocused. The browser PWA path is unchanged.
