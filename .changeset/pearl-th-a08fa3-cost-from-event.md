---
"@smooai/smooth": patch
---

bench: deterministic cost extraction via JSON sidecar. `score-tui` was reporting $0.00 across every task whenever the TUI pane-scrape regressed (status-bar format drift, ratatui repaint race against `tmux capture-pane`, ANSI bleed, in-flight `Completed` event). Now: `smooth-code` writes a `{cost_usd, iterations, ts_unix_ms}` JSON sidecar on `AgentEvent::Completed` when `SMOOTH_BENCH_COST_SIDECAR` is set, atomically (tmp→rename) and best-effort. `smooth-bench/tui_score` sets the env var to a per-task path under the run dir before spawning `th code`, then prefers the sidecar over the legacy pane-scrape. Falls back to scrape for older `th` binaries; falls back to $0.00 + a loud warning if both miss. Opt-in via env so plain `th code` sessions never drop a sidecar in the user's cwd. Pearl th-a08fa3.
