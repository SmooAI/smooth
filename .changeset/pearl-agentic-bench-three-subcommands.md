---
"smooth": minor
---

Three new `smooth-bench` subcommands for measuring agentic coding quality (the real `smooth-coding` decision data):

- **`score-swe-bench --variant verified|lite`** — SWE-bench Verified / Lite (Princeton). 500 real GitHub issues from popular Python repos with held-out FAIL_TO_PASS + PASS_TO_PASS test suites. HuggingFace dataset fetch + atomic JSONL cache at `~/.smooth/bench-data/`. Score-compatible output bucketed under `"python"` for compare with the polyglot path. Industry-comparable headline number.
- **`score-real --tasks-dir ...`** — Multi-axis benchmark on curated mini-projects in our stack (Rust + Python + TS). Each task ships a `workspace/`, hidden-tests/, and a `grade.toml` declaring weights for pass / edits / verify / tools / cost. Scorer combines them into a weighted-mean per task. First task shipped: `rust-ttl-cache` (TTL-cache wrapper around an HTTP client). Four more proposed in `tasks-real/README.md` as TODOs.
- **`score-replay --repo owner/repo --since YYYY-MM-DD`** — Auto-harvest tasks from real merged PRs via `gh pr list --json`. For each PR ≥3 files + ≥1 test file: clone the parent commit, feed the PR title + body as prompt, score by whether the agent makes the same tests pass that the human PR did. Trait-injected (`GhCli` / `RepoFetcher` / `ReplayDriver`) so the unit tests use a `SeedFetcher` instead of real `gh` calls.

All three reuse the existing `tui_score` dispatch (driving `th code` via tmux with the coach driver + VERIFY rule). 277 lib tests pass (86 new). Built via parallel agent workflow (`wf_31decba4-b81`) — three isolated worktrees, then merged + CLI wired by hand. Live `th code` dispatch wiring for each is left as TODO in the agents' notes — the scoring + dataset + harvest + grading infrastructure is what landed here.
