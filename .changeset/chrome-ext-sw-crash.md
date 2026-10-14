---
'@smooai/smooth': patch
---

`th chrome`: the extension's service worker no longer crashes on a fresh install. It touched `chrome.debugger` at startup, which doesn't exist until trusted input is granted, so the extension never connected to `th`. Adds a ringed `th` mark (the Smoo AI avatar's gradient band) as `images/th-ring-icon.svg`.
