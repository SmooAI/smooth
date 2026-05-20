---
'@smooai/smooth': patch
---

Pearl th-7fdfa9: fix two harness bugs in `smooth-bench score-tui`
that produced the same false-pass smell as th-f46efa.

1. **`tmux send-keys -l` mangles newlines into `j`.** Every `\n` in a
   multi-line task prompt was being interpreted as the `C-j` keysym
   and, in literal mode, degraded to the bare letter `j`. The pearl
   debug log showed task prompts rendering as
   `affine-cipher (python).jjWorking directory: …jFiles present:j  -
   INSTRUCTIONS.mdj…`. Switched `TmuxDriver::send` to
   `load-buffer` + `paste-buffer`, which inserts the payload as raw
   bytes — newlines, tabs, and Unicode all preserved verbatim. Added
   a regression test that pipes a 3-line message through `cat >
   tmpfile` and asserts the file contains exactly 3 lines with no
   stray `j`s.

2. **Driver LLM uses Claude-Code-style slash commands.** The
   default driver model (`smooth-summarize`) was emitting `/open`,
   `/read`, `/help` instead of plain English, which the TUI
   rejected as "Unknown command" and in two cases accidentally
   fired skills (`/add-show`, `/create-skill`). Hardened the system
   prompt + user prompt with explicit "no slash commands; you have
   no file/shell access — ask the assistant in plain English"
   directives, and added a slash-command guard in `run_human_loop`
   that drops `/`-prefixed turns, logs the violation to the
   pane-debug log, and re-asks the model with a reinforcement
   prompt. After 3 consecutive slash turns the loop bails with
   `LoopExit::Stuck` instead of burning the full turn cap.

Tests added: `tmux_driver::send_preserves_newlines_no_j_leakage`,
`human_driver::run_human_loop_marks_stuck_after_three_slash_commands`,
`human_driver::run_human_loop_accepts_plain_english_message`, plus
prompt-construction unit tests asserting the no-slash-commands
language is present.

Verified with a single-task `score-tui --pr --task-limit 1 --debug`
smoke run: the new pane log shows the initial task prompt rendering
with real newlines (zero `j` artifacts) and the driver's follow-ups
all plain English (zero `/`-prefixed turns). The task itself failed
(affine-cipher is a hard single-attempt task), but the harness is
now healthy.
