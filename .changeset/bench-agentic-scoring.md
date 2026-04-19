---
"smooai-smooth-bench": minor
---

Rip out the per-language test-output parsers (`parse_pytest`, `parse_cargo_test`). Scoring now runs the language's test command and hands the stdout to the `smooth-judge` routing slot with a strict JSON-only contract — works for pytest, cargo test, go test, jest, gradle, ctest, anything. `parse_judge_response` is unit-tested for code fences, prose-wrapped JSON, partial totals, and malformed output; the LLM call itself is `judge_test_output` and can be called directly by other callers.
