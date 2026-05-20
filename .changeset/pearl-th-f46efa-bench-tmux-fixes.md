---
'@smooai/smooth': patch
---

Pearl th-f46efa: fix `smooth-bench score-tui` tmux harness so it
actually exercises `th code` instead of false-passing.

PR #55's first `--pr` run finished 18 tasks in 12 minutes with
2/18 pass and $0.00 cost — strong evidence the harness was broken,
not Smooth. The score-tui-pr.log showed "no server running on
/private/tmp/tmux-501/default" twice before every task and a
median task wall-clock of 38s, far below the 900s per-task cap.
The two Rust passes were false positives: aider-polyglot fixtures
should not pass un-edited, so the harness was scoring workspaces
the agent never touched.

Root causes addressed:

1. **Empty-pane false-idle in `wait_for_idle`**: the old heuristic
   ("byte-identical for 2s") declared a blank pane idle, so the
   LLM-as-human loop sent its first turn before `th code` had
   finished booting. `wait_for_idle` now takes a `min_bytes` floor
   (default 200 non-whitespace chars) — below the floor the pane is
   treated as still-rendering and we keep polling. New
   `wait_for_idle_with_floor` exposes the floor explicitly for
   tests.
2. **Stale-state false-render in `wait_for_first_render`**: the gate
   accepted a single printable char as "rendered". Now requires
   the same 200-char floor before returning, so a brief artifact
   doesn't count.
3. **`th code` boot timeout too short**: bumped default
   `TuiTaskConfig::boot_timeout` from 15s → 120s. `th code` brings
   up the Safehouse microVM + cast (wonk · goalie · narc · scribe
   · archivist · diver · groove) + operator-runner pool before the
   input prompt; empirically 30-60s on a warm machine. 15s was
   under, so the boot gate fired prematurely.
4. **Tmux stderr noise**: `tmux has-session`, `tmux -V`, and the
   Drop's `kill-session` all printed "no server running" to stderr
   in the no-server-yet case (normal during probing). All probes
   now redirect stderr to `/dev/null`; real failures still surface
   the error text through `capture-pane`'s embedded stderr-in-error.
5. **Stuck tasks were scored as passes**: aider-polyglot fixtures
   should not pass un-edited. New `stuck_means_failed` knob (on by
   default; bypass with `--allow-stuck-passes`) forces
   `solved=false` when the LLM-as-human driver bailed on turn 1
   without a `TASK_COMPLETE` sentinel — kills the silent
   corruption where un-edited Rust workspaces reported as solved.
6. **$0 cost across the board is now a loud warning**: the harness
   prints a warning at the end of a sweep when every task reports
   $0.00, so future-us can't mistake an un-wired cost surface for
   "the run was meaningful but cheap".

Diagnostics (`--debug`):

- New `PaneDebugLog` type writes per-task `<lang>-<task>.pane.log`
  to the run dir with timestamped records at every `send`, every
  `wait_for_idle` boundary, AND the boot screen frames.
  `capture-pane` failures dump the last good capture so the op can
  see what the user saw before the session died.
- New `--task-limit N` flag caps the sweep at N tasks (default 0 =
  no cap). Use `--task-limit 1 --debug` to exercise a single task
  end-to-end with full pane logging.

Tested: existing tmux integration tests updated for the new
boot-floor + 200-char idle threshold; new tests cover the floor
rejecting empty panes, the debug log recording send/idle events,
and the duplicate-session / drop-kills-session paths still pass
with longer payloads.
