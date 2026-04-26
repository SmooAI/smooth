---
"@smooai/smooth": minor
---

**TEST phase + self-validating EXECUTE + loop v2.**

New TEST phase runs AFTER the provided tests pass. Classifies the code (React component / API client / web flow / WebSocket / DB service / CLI / pure library / async code), picks the canonical test stack for that shape (MSW, Playwright, testcontainers, property-based via hypothesis/proptest/fast-check, …), installs missing deps, and writes boundary-pushing tests that exercise real behaviour — not another unit test, but MSW intercepting the actual `fetch` retry loop or a Playwright browser clicking through the actual flow. If its new tests reveal real bugs, the workflow loops back to EXECUTE with them as the next review findings; if they're all green the workflow moves on to FINALIZE. Routed through `smooth-reviewing` (adversarial test writing is closer to code review than fresh implementation). Skippable via `SMOOTH_WORKFLOW_SKIP_TEST=1` for benchmark runs where adding extra tests would change the score.

EXECUTE prompt now demands the agent pick a **self-validation** check appropriate to the language (`cargo check`, `python -m py_compile`, `go vet`, `node --check`, `tsc --noEmit`, etc.) and run it before declaring done — no more handing off to VERIFY with code that won't compile. Agent-written tests are welcome but MUST land with their implementation in the same change (no orphan failing tests that reference unimplemented methods).

Loop v2 stop conditions are budget + plateau, not a fixed iteration cap. `verify_signature` extracts pass/fail counts from each VERIFY and breaks early when the signature repeats (model going in circles). Budget short-circuit breaks when the next cycle would likely blow the cap. Default `max_outer_iterations` bumped 3 → 10 as a ceiling, not the governor.

New thesaurus phrases for the TEST phase — "Writing tests…", "Mocking the network…", "Booting the browser…", "Red-teaming the code…", etc. Status-bar cycle includes them when TEST is active.
