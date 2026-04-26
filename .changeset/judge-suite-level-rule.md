---
"@smooai/smooth": patch
---

Bench: judge prompt and test commands tightened for suite-level summaries. Drop `cargo test --quiet` and add `-v` to `go test` so each runner emits per-case lines. Judge system prompt now has explicit scoring rules — `ok <package>` with no per-case detail maps to passed=1/total=1 instead of returning all zeros, which previously marked a passing Go suite as UNSOLVED. Build errors count as failed=1.
