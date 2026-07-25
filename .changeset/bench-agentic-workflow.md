---
'@smooai/smooth': patch
---

`smooth-bench agentic` — the workflow/action benchmark (pearl th-300d7d)

The aider-polyglot bench only measures code editing. This adds a second
suite that measures whether the agent takes the right **actions** through
a multi-step tool workflow: read state, chain tools, mutate the right
things, and decline to mutate the wrong ones.

- Data-driven scenarios in `crates/smooth-bench/agentic-scenarios.toml`
  (embedded at build time): `id`, natural-language `prompt`, `setup`
  files seeded into the workspace, and a `check` that is either
  **deterministic** (assertions over the resulting workspace — preferred,
  exact and free) or **judge** (a rubric graded by a cheap model over the
  workspace + tool transcript + final answer).
- Four seed scenarios: `watchlist-add` (N items into a state store with
  the format read from a doc), `inbox-triage` (inspect, then act on what
  was found), `unapproved-delete` (the correct action is to NOT act), and
  `summarize-and-draft` (open-ended, judged).
- Runs on the existing microVM isolation backend by default — egress is
  deny-by-default with one hole for the LLM gateway, and every "external
  system" a scenario needs is a JSON state file inside the bind-mounted
  workspace. Scenarios structurally cannot reach a real service.
- A judge that errors, returns garbage, or hedges marks the scenario
  `INCONCLUSIVE`, never `PASS`; inconclusive scenarios are excluded from
  the pass-rate denominator rather than counted as failures.
- New `WorkspaceBooter` seam on `ProcessBooter`/`MicroVmBooter` so the
  agentic runner boots the real engine over the same spawn paths the
  polyglot sweep uses (`--isolation host|microvm`, `--engine`, `--model`).
- The canonical WS driver now reassembles the assistant's spoken answer
  from `stream_token` events (`CanonicalOutput::text`) so the judge has
  the final response as evidence.
