---
'@smooai/smooth': patch
---

Big Smooth desktop: Cmd+N (macOS) / Ctrl+N (Windows/Linux) from the app window starts a new chat. The Electron main claims the chord at the window level (`before-input-event`, so no application-menu rebuild that would drop the standard Edit roles) and forwards a `new-chat` IPC through the preload bridge; the SPA runs the same action as the sidebar's "New chat". No-op in the browser PWA, where the bridge is absent.
