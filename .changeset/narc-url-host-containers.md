---
'@smooai/smooth': patch
---

Fix `host_tool` connectivity from inside the sandbox: the runner now
reaches Big Smooth's `/api/host/exec` at
`http://host.containers.internal:4400` in both boardroom and
host-modes. Previous code fell back to `http://127.0.0.1:4400` in
host-mode, which inside the microsandbox VM means the SANDBOX'S
loopback — not the host's — so every `host_tool` call failed with
"error sending request for url (http://127.0.0.1:4400/api/host/exec)".
The boardroom/host-mode distinction was a red herring; microsandbox
exposes the outer host under `host.containers.internal` in both
modes. `SMOOTH_NARC_URL` env override still wins.
