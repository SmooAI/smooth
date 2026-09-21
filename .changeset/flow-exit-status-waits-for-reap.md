---
'@smooai/smooth': patch
---

SmoothFlow records each session's exit code itself instead of relying on tmux, which can take seconds to learn it. A small `sh` wrapper now runs the harness as a child, writes its exit code to a per-launch file, and exits with the same code. A crash card shows the real code ("exit 2", or "killed by signal 9") instead of "exit -1". A clean exit is no longer resumed as a crash. When no code can be read at all, the session is shown as "exit status unknown" and is not resumed. Ctrl-C still reaches the harness, and the wrapper outlives it (th-7ff336).
