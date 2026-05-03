---
"@smooai/smooth": patch
---

operator runner: stop the moment tests pass — no over-iteration

Empirical discovery from the M-workstream slot sweep: `glm-5.1` solved
python/affine-cipher correctly (16/16 tests pass) within ~5-10 minutes of
dispatch but kept iterating for 33+ minutes before posting `[IDLE]`. Same
pattern observed in take 7 with kimi-k2-thinking (25 min), and likely in
every "real solve" of take 7. The model lands a working answer, the test
suite exits 0, and then the model keeps editing — refining, re-verifying,
documenting, "improving" — until the iteration cap or some long quiet
finally fires.

Root cause: the D1 system prompt's "Verify before claiming done" block
told the model to verify but not to *stop verifying* once green. Models
respect that ambiguity by continuing.

Fix: replace the soft "Only then declare complete" with an emphatic STOP
rule. Bench tasks that previously took 25-30 min should now finish in
the 2-5 min range — the time it actually takes the model to write a
correct solution and run the suite once.

Models are fine. The prompt was the bottleneck.
