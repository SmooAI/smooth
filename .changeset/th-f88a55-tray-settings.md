---
'@smooai/smooth': patch
---

Big Smooth desktop: legible white tray icon + connect switcher in Settings (pearl th-f88a55).

- **Tray icon** — the menu-bar `th` mark was teal (low-contrast on a dark bar) and not a macOS template image. It's now a black template (`setTemplateImage`), so macOS renders it white on a dark menu bar and black on a light one — always legible, adapting to the theme.
- **Connect in Settings** — the daemon switcher (This Mac / discovered tailnet daemons like smoo-hub) was tray-only. Settings → Connection now has it too, driven by a minimal Electron preload/IPC bridge (`window.bigSmooth`: `listDaemons` + `connectTo`). In the browser PWA (no bridge) it degrades to a pointer at the menu-bar app. Note the switcher renders from the *served* SPA, so it appears wherever that daemon's build includes it.
