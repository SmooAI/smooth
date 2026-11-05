---
'@smooai/smooth': minor
---

SMOODEV-4269: SmoothFlow can open a shell or agent session on another machine over the system `ssh`, from the app's New Shell and New Session sheets, `th flow new --ssh <host>` and `flow_new`, and it shows in the fleet like any other session. `~/.ssh/config`, agents and `ProxyJump` work, and nothing is installed remotely. The remote program lives as long as the connection: quitting the app keeps it, a dropped connection usually ends it, a lost shell stays closed and a lost agent waits for you to resume it. Hosts starting with `-` or carrying a password are refused. See SmoothFlow.md, Remote sessions.
