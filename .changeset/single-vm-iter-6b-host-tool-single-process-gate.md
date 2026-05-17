---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 4 iter-6b. Host-tool gate. In single-VM
mode the bundled CLIs (gh, aws, gcloud, az, kubectl, docker)
are right there in the VM and the host-stub mints credentials
over UDS — the legacy `host_tool` indirection through Big
Smooth's `/api/host/exec` endpoint is unnecessary.

operator-runner now skips `host_tool` registration when
`SMOOTH_SINGLE_PROCESS=1`. The agent falls through to
`BashTool` for the same CLIs, still mediated by Wonk's
`check_cli` + Narc audit just like every other shell call.
Logs the skip ("CLIs run directly in-VM") so the path is
visible from the runner output.

Legacy multi-VM dispatch (no `SMOOTH_SINGLE_PROCESS`) is
unchanged — the existing host_tool path stays live.

No new tests — single behavioral gate, exercised end-to-end
by the iter-3g smoke test once the runner is co-resident
with BS in Phase 2. Will retire `host_tool` entirely in a
later iter once the legacy path is gone.
