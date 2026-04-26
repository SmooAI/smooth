---
"@smooai/smooth": patch
---

`SmoothDoltServer::spawn` now launders the spawn through `/bin/sh -c 'exec setsid smooth-dolt serve ...'` with a cleared env, instead of `Command::new(smooth-dolt)` directly. The embedded Dolt engine inside `smooth-dolt serve` cannot run when its parent process is the long-running Big Smooth daemon (under launchd) — even with stdin/stdout/stderr all set to `/dev/null` it parks all goroutines in `pthread_cond_wait`. The intermediate shell + `setsid` detaches the new server into a fresh session, drops anything weird Big Smooth's tokio runtime had attached to the spawn, and the embedded Dolt comes up clean. Verified on smoo-hub: `/api/projects` now responds in <1s where it previously hung at 60s+.
