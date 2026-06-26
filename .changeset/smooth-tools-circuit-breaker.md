---
'smooai-smooth-tools': minor
---

EPIC th-c89c2a: the `bash` tool now hard-denies **circuit-breaker** commands
(`smooth_tools::is_circuit_breaker`) before spawning — `rm -rf /` / `rm -rf ~`,
fork bombs, `curl … | sh`-style remote-code-execution, `mkfs`/`dd of=/dev/…`.
Mirrors the daemon's `permission.rs` circuit-breaker so the operator local-flavor
path (which doesn't install the bespoke permission engine) still gets a deny gate.
The kernel OS-sandbox remains the load-bearing boundary; this is cheap
defense-in-depth. (Closes th-1f694a — delivered in-tool rather than via an
operator host-hook seam, which the sandbox made unnecessary.)
