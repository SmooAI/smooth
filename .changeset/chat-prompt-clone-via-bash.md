---
"@smooai/smooth": patch
---

Stronger chat-agent prompt — clone goes through bash, not a teammate

Even after adding the bash carve-out for one-shot writes, the chat
agent kept reaching for `teammate_spawn` on `git clone` requests
because the rule was buried mid-prompt. Reorganized the prompt around
a numbered "decision rules" block at the top with rule 1 being
"clone/fetch/mkdir → bash, NOT teammate_spawn" — explicit, ordered,
non-negotiable.

Also tightened `teammate_spawn`'s tool description:
- Lead sentence is now "for REAL CODING WORK ... do NOT use this for
  one-shot bash-allowlist commands". Models are likelier to skip a
  tool whose schema says "don't use for X" than to read past five
  paragraphs to find the same caveat.
- The `model` parameter description explicitly warns against
  `smooth-fast-gemini` (it can't reliably emit native tool calls and
  wedges the runner) and removes the prior advice to use it for
  read-only lookups, which was the trigger for the 5-min wedge this
  morning.
- The `working_dir` field's description explicitly says "never pass a
  directory as broad as ~ or /". The wedge happened with
  `working_dir=/Users/brentrager`.

Verified end-to-end: `clone brentrager/budgeting to
~/dev/brentrager/budgeting` now answers in ~47 s with the repo
actually cloned (verified via `ls .git` on the destination).
