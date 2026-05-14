---
'@smooai/smooth': patch
---

bench: permission-flow scenarios + headless `--auto-approve` flag.
Closes the auto-mode work queue. `scenario.toml` gains an
`auto_approve` meta field (default: `deny`) and a new
`kind = "permission"` assertion that pins the expected resource +
resolution scope. `th code --headless --auto-approve <mode>`
spawns a tokio task that polls `/api/access/pending` and resolves
each Ask per the configured mode — unattended runs are safe by
default (every Ask becomes a deny) but can opt into permissive
modes for bench scenarios that need them. 11 new tests across
`scenarios::AutoApprove` parse/serde/round-trip + `auto_approve`
module (fake-Big-Smooth integration for each mode, sentinel-drop
stops the loop). Pearl th-400773.
