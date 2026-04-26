---
"@smooai/smooth": minor
---

New internal crate `smooai-smooth-bench` — benchmark harness for Aider Polyglot single-task runs. Not part of the user-facing `th` binary; invoke via `cargo run -p smooai-smooth-bench --` or `scripts/bench.sh`. Dispatches to Big Smooth over the headless WebSocket path, runs the language's test command in the scratch work dir, and writes a scored `result.json` to `~/.smooth/bench-runs/<run-id>/`. Parsers for pytest and `cargo test` summaries included; Go / JS / Java / C++ command shapes wired but not exercised yet. SWE-bench, Terminal-Bench, batch mode, and the web scoreboard are separate pearls.
