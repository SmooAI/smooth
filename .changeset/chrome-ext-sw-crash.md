---
'@smooai/smooth': patch
---

`th chrome`: the extension's service worker no longer crashes on a fresh install. It touched `chrome.debugger` at startup, which doesn't exist until trusted input is granted, so the extension never connected to `th`. The popup is redesigned in Presence: a connection pill, a problem box that says how to fix it, and switches for Agent browsing and Trusted input.

New ringed app icons: the `th` mark (Big Smooth, the web app, the Chrome extension) and the SmoothFlow mark now sit inside the Smoo AI gradient band. `scripts/gen-ring-icons.py --assets` renders every icon from the SVG masters.
