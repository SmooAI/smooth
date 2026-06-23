---
'smooai-smooth-daemon': patch
'smooai-smooth-web': patch
---

Phase 4 Slice 5 (th-bd0def): cancel a running task from the control
surface. The daemon's `TaskCancel` handler now emits a terminal
`TaskError("task cancelled")` and resets the session to idle when it
actually aborts a fiber — previously the aborted task skipped its
completion cleanup, leaving the client stuck "busy" with no signal. The
control surface captures the running `task_id` from the event stream and
shows a **stop** button while a task runs, sending `TaskCancel`.
