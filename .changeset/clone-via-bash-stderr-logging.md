---
"@smooai/smooth": patch
---

Direct git-clone via bash + runner stderr logging

Two fixes for the "ask BS to clone a repo, watch the spinner for 5
minutes, get a wall error" failure mode:

- **System prompt**: one-shot allowlisted writes (`git clone`, `gh repo
  clone`, `mkdir -p`, `curl -o`) are now explicit `bash` territory.
  Previously the prompt blanket said "writes → spawn a teammate"
  which sent a 2 s clone through a 30-90 s teammate boot path that
  could (and did) wedge.
- `mkdir` added to the bash allowlist; bash timeout bumped 10 s →
  30 s so a small clone fits.
- **Runner observability**: `dispatch_ws_task_direct` now logs
  `tracing::info!` on spawn (PID + binary + cwd + model) and on the
  first stdout line. Runner stderr is mirrored to `tracing::warn!`
  so a wedge that prints a panic / init error is visible in
  `service.log` instead of disappearing into a WebSocket TokenDelta
  no one is reading.
