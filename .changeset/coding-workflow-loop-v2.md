---
"smooai-smooth-operator": patch
"smooai-smooth-operator-runner": patch
---

CodingWorkflow loop v2: stop conditions are budget + plateau, not a fixed iteration cap. Default `max_outer_iterations` bumped 3 → 10; the real governor is `verify_signature`, which extracts pass/fail counts from each VERIFY and breaks early when the signature repeats (model going in circles). Budget short-circuit added too — if next iteration would likely blow the cap, break. `verify_signature` is unit-tested across pytest/cargo/go/jest summaries, compile-error lines, and progress deltas.
