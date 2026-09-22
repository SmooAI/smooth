---
'@smooai/smooth': patch
---

A daemon that is slow to answer is no longer treated as dead (th-4af55f). The `/health` probe used to count any error, including a 750 ms timeout, as "nobody's there". So a SmoothFlow daemon slow under load lost `~/.smooth/flow.addr` to a second daemon, and the single-instance check could start a daemon next to a slow older one. Now only a refused connection, or a non-2xx answer from whatever is listening, counts as dead immediately. A timeout triggers a second probe a second later with a 3 s timeout, and only a second failure counts as death.
