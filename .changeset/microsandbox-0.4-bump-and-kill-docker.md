---
'@smooai/smooth-cli': minor
---

Bump microsandbox 0.3.14 → 0.4.6 and rip out the Docker sandbox backend.

**microsandbox 0.4.6** brings:
- PR #673 — bounded relay handshake reads + `boot-error.json` on timeout. Failed boots now surface a structured error instead of the opaque "sandbox process exited before sending startup info" that hid the real cause on the previous 0.4.5 attempt.
- PR #650 — `exec.log` capture + typed `ExecFailed`.
- PR #697 — SIGKILL on `replace`-grace overruns, relevant to the bind-mount silent drop tracked in pearl th-dd0cef.

Verified end-to-end on macOS HVF: `th up` boots the boardroom microVM, `:4400` returns HTTP 200, `th down` cleans up with no leaked `msb`/`krun` processes.

**Docker backend removed.** `DockerSandboxClient`, `SMOOTH_SANDBOX_BACKEND=docker`, and `SMOOTH_DOCKER_BIN` are gone. Smooth has exactly two modes now — `th up` (sandboxed via microsandbox) and `th up direct` (host process, only safe in a pre-trusted environment). Docker is still callable from inside the sandbox when reaching out to host Docker / OrbStack / Colima for nested-virt-free workloads; it's just not a sandbox runtime for Smooth itself.
