---
'@smooai/smooth': patch
---

smooth-flow: sessions record the daemon that created them (`owner`), and a daemon's supervision tick only touches its own rows. Two daemons sharing one `flow.db` (the default `th up` daemon and the SmoothFlow app's child, or an orphaned instance) no longer mark each other's live panes `dead · process vanished` and race to relaunch them (th-4f7866).
