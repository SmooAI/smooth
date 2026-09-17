---
'@smooai/smooth': patch
---

SmoothFlow 0.2.3 — starting a session stops being paperwork. The New Session screen no longer demands a pearl id, a Jira key, or a worktree up front: pick a kind, type what you want done, hit Start, and SmoothFlow infers the rest from the prompt and the repo. Plain `claude` and `codex` sessions you started yourself are adopted into the fleet instead of sitting outside it (th-c103c1, #594). Four more agent CLIs launch out of the box — aider, goose, crush, and cline — on top of a declarative scrape-rule system that replaces the hand-written per-harness pane parsers, so teaching SmoothFlow a new CLI is now a manifest edit rather than Rust (th-e77603, #599). Also fixes aider dropping the first prompt: it needs a beat after its banner before the paste lands, and it now gets one (th-d2a1e4). th-2f31d8.
