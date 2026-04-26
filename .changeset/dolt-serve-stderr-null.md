---
"@smooai/smooth": patch
---

`SmoothDoltServer::spawn` now also sets stderr to `/dev/null`. Inheriting the parent's stderr (which under launchd points at `~/.smooth/service.err`, a regular file) wedges the embedded Dolt engine inside `smooth-dolt serve` — SQL queries park forever in `pthread_cond_wait`. The shell-spawned binary works fine because the shell connects stderr to a TTY or `/dev/null`. Verified on smoo-hub: same binary, same dolt dir, only difference is the inherited stderr fd.
