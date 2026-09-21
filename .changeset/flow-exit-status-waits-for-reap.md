---
'@smooai/smooth': patch
---

SmoothFlow: a crashed agent's card shows its real exit code instead of `exit -1`. tmux marks a pane dead as soon as its pty closes, but only knows the exit status once it has reaped the process, so the supervisor now waits up to 3 s for that status before settling for "unknown". The flow_e2e crash tests failed intermittently on CI because of this gap (th-7ff336).
