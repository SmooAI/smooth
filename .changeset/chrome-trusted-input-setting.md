---
'@smooai/smooth': patch
---

`th chrome`: Trusted input works. Chrome won't accept `debugger` as an optional permission and silently dropped it, so the switch could never turn on. `debugger` is now declared up front, and Trusted input is a setting in the extension: off by default, it needs browsing, and turning it off detaches every debugger session. After upgrading, run `th chrome install` and reload the extension.
