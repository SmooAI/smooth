---
'@smooai/smooth': patch
---

th attest: fall back to a local run (fast, with a warning) when the build box is unreachable

`th attest rust` routes to a remote build box. When that box was unreachable the ssh
had no `ConnectTimeout`, so it stalled on the TCP handshake before finally blocking the
row and handing it to CI — the slow path the build box exists to avoid.

Now: both ssh invocations use `ConnectTimeout=10`, so an unreachable host surfaces in
seconds; and a remote infrastructure failure (unreachable / no disk / dropped) no
longer blocks — it prints a visible `⚠ <host>: <reason>` warning and runs the check
**locally** instead, producing a real verdict on this machine rather than deferring to
CI. Slower (no warm cache), which is exactly why the warning is loud.
