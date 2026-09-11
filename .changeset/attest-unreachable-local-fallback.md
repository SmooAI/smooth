---
'@smooai/smooth': patch
---

th attest: never stall or hang on the build box — fail fast, bound the run, fall back to local

`th attest rust` routes to a remote build box. Three ways that stalled or hung `th`,
now closed:

- **Unreachable box:** the ssh calls had no `ConnectTimeout`, so they stalled on the
  full TCP handshake. Both now use `ConnectTimeout=10`.
- **Wedged mid-run:** once connected, a check could wedge (measured: cargo finished but
  `rust.sh` hung in a docker probe) and `child.wait()` never returned, hanging `th`
  with it (th-7db71c). A watchdog now kills the ssh after a deadline
  (`SMOOTH_ATTEST_REMOTE_DEADLINE_SECS`, default 45 min); killing the local client drops
  the connection so the remote check is torn down too.
- **Either way:** a remote infrastructure failure (unreachable / no disk / dropped /
  timed out) no longer blocks and defers the whole row to CI. It prints a visible
  `⚠ <host>: <reason>` and runs the check **locally** — a real verdict on this machine.
  Slower (no warm cache), which is why the warning is loud.
