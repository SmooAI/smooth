---
'@smooai/smooth': patch
---

`flow.hello`'s `daemon` block carries the daemon's `home` (th-89eb13). A phone, or a client of a daemon on another machine (WSL, a remote host), can now title a session in that home `~` instead of showing the folder name. The field is optional and older clients ignore it. SmoothFlow for Mac and for Linux/Windows abbreviate against it, and the mock server sends it.
