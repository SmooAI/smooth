---
"@smooai/smooth": patch
---

Fix smooth-dolt subprocesses hanging indefinitely when called from inside Big Smooth's tokio runtime. Root cause: smooth-dolt's Go runtime forks a long-lived `dolt sql-server` child that inherits the parent process's open file descriptors. When `SmoothDolt::run` connected stderr to a pipe (the default behaviour of `Command::output`), the daemon child held that pipe fd open after smooth-dolt itself exited; `Command::output` waited for EOF on the pipe forever. Observed on smoo-hub as `/api/projects` timing out at 60s+ while the same command from a TTY returned in 50ms. Fix is to redirect smooth-dolt stderr to `/dev/null` (`Stdio::null`) so there's no pipe to inherit; on non-zero exit we now surface "rerun the CLI for stderr" instead of the captured message.
