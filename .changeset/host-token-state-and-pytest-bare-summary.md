---
'@smooai/smooth': patch
'@smooai/smooth': patch
---

Fix two small bugs: a UB-prone env mutation in Big Smooth and a missed pytest
summary shape in the bench scorer.

**Big Smooth — host-tool bearer moved off process env (pearl th-87dfee).**
`AppState::new` mutated the `SMOOTH_HOST_TOKEN` process env var via
`std::env::set_var`, which runs after the tokio runtime is up. On glibc,
`set_var` racing a `getenv` on another thread can segfault, and Rust 2024 marks
`set_var` unsafe for exactly this reason. The token now lives on `AppState`
(`Arc<str>`), seeded once from an inherited value or freshly generated. The
`/api/host/exec` handler reads it from state; dispatch clones it into the
operative's child `Command.env` (setting a child's env is sound — only the
in-process global mutation was UB).

**Bench scorer — recognize bare pytest summaries (pearl th-19ab7c).**
`parse_pytest_summary` required a leading `=` decoration, so it missed the
all-pass form `16 passed in 0.01s` that pytest emits when it can't detect
terminal width (output piped to a file — our exact capture path). Those runs
fell through to the LLM judge on every all-pass task. The parser now strips the
optional `====` bars and recognizes any line reporting a status keyword and
ending in a pytest duration, covering passed-only, failed-only, mixed,
skipped/warnings, and `no tests ran` (scored 0/0/0, not a pass).
