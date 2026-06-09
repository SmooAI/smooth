---
'@smooai/smooth': patch
---

Cut smooth over to the published `smooai-smooth-operator-core` v0.14.0 (crates.io); re-home the th-code harness into smooth's own crates

This is the final PR of the engine-decouple program (SMOODEV-1790, PR 4/4). The
engine `smooai-smooth-operator-core` is now published on crates.io at `0.14.0` —
a clean, GENERIC agent engine with the `th code` coding harness REMOVED.
Previously smooth depended on the engine via a git rev (`bb9a256`) that still
carried the harness, which is why it kept building.

- **Engine dep switched to crates.io 0.14.0.** Root `Cargo.toml`:
  `smooth-operator = { git = …, rev = "bb9a256…" }` →
  `smooth-operator = { version = "0.14.0", package = "smooai-smooth-operator-core" }`.
  The dep KEY stays `smooth-operator` so the `use smooth_operator::…` imports for
  the generic engine API are unchanged. `Cargo.lock` now resolves the engine from
  `registry+https://github.com/rust-lang/crates.io-index` (checksum-pinned), not a
  git source — the git-rev bridge is gone.

- **New `smooth-cast` crate** re-homes the bits the engine dropped, built on the
  engine's generic public API (`Agent`/`ProviderRegistry`/`ToolRegistry`/generic
  `Cast`/`OperatorRole`/`Clearance`):
  - `coding_workflow` — the `th code` single-agent outer loop
    (`run_coding_workflow`, `task_text_has_cleanup_intent`, …).
  - `skills` — skill discovery (`discover`, `SkillScope`, `SkillSource`, `Skill`)
    plus the built-in `create-skill` skill.
  - `cast` — the four coding-harness cast roles the generic engine no longer ships
    (`fixer`, `oracle`, `chief`, `intent_classifier`), and a `cast::builtin()` that
    returns them on top of the engine's generic built-in roles. All moved tests came
    with the code.

- **Consumers repointed** to `smooth-cast`: `smooth-operative` (coding_workflow +
  `fixer` role resolution), `smooth-code` (skills + `chief`/`intent_classifier`
  routing), `smooth-cli` (skills + `--agent` role resolution), `smooth-bigsmooth`
  (skills + session auto-naming). Every site that did `Cast::builtin().get("fixer"|
  "oracle"|"chief"|"intent_classifier")` now uses `smooth_cast::cast::builtin()`.

- The Big-Smooth reporter hooks the engine also dropped stay deleted — verified
  zero smooth consumers (`with_reporter`/`BigSmoothReporter`/`ReporterEvent`/
  `report_to_bigsmooth`/the `bigsmooth` engine feature). smooth's own
  `smooth-bigsmooth` gRPC crate is unrelated and untouched.
