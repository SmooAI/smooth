---
"smooai-smooth-bench": patch
---

Bench: enable-skipped-tests step. Aider Polyglot tasks intentionally ship with most of their tests disabled so the stub code compiles — Rust bowling has 30 of 31 marked `#[ignore]`, JS bowling has 29 `xtest`/`it.skip`/`test.skip`/`xit`/`xdescribe`/`describe.skip` variants. Without flipping these on, the harness scored a "solved" verdict off a single trivial case. Rust now runs `cargo test -- --include-ignored`; JS spec files get their skip markers rewritten (`xtest(` → `test(`, etc.) in the scratch dir before tests run. Source dataset is untouched; only the per-run copy is edited.
