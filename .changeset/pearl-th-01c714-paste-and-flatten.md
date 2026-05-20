---
'@smooai/smooth': patch
---

Pearl th-01c714: stop multi-line task prompts from fragmenting into
N `You:` submissions in the `smooth-code` TUI when driven by the
bench harness.

After pearl th-7fdfa9 fixed the `j`-for-newline bug in
`tmux_driver::send`, newlines now correctly survive into the TUI's
input box — but the TUI's input handler treats every `\n` as Enter
(submit). So a 13-line task prompt arrived as 13 separate `You:`
submissions instead of one, fragmenting the conversation. Evidence:
`~/.smooth/bench-runs/80c092b0/python-affine-cipher.pane.log`.

Two-pronged fix (belt-and-suspenders):

1. **Bracketed paste in `tmux_driver::send`.** Added `-p` to the
   `tmux paste-buffer` invocation so the content is wrapped in
   `\e[200~ ... \e[201~` markers. Bracketed-paste-aware TUIs use
   these markers to keep embedded newlines as soft newlines rather
   than treating each as Enter. If the receiving application has not
   enabled `\e[?2004h`, tmux strips the markers and behaviour is
   identical to the prior non-`-p` path — so `-p` is a safe upgrade.

2. **Flatten multi-line prompts before sending.** Reformatted
   `lib::build_prompt` to produce a single line (semicolon-joined
   clauses). Added `human_driver::flatten_for_tui` which collapses
   newlines to ` | ` and is applied to the initial task prompt seed
   and every driver-model follow-up before `driver.send`. Even if
   the TUI never honors bracketed paste, the flattened form is
   guaranteed to land as one `You:` block. Cheap insurance against
   future TUI input-handler changes.

Tests added:

- `lib::build_prompt_is_single_line` — asserts no `\n`/`\r` in the
  bench task prompt.
- `human_driver::flatten_for_tui_*` (5 cases) — covers passthrough,
  trimming, empty input, blank-line dropping, and the multi-line
  → pipe-separated transformation.

Verified with `score-tui --pr --task-limit 1 --debug` against a
running Big Smooth: the new pane log shows the seeded prompt
landing as a single `You:` block containing the full text, instead
of one `You:` block per line as before. The task itself still
fails (affine-cipher under single-shot constraints is hard), but
the harness now sends what we intend it to send.
