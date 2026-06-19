---
"@smooai/smooth": patch
---

bench: deterministic test-result regex scorer + forensic dump. Adds `parse_native_test_summary` with per-language parsers (cargo's `test result:`, pytest's `N passed, N failed`, jest summary) that run **before** the LLM judge — the judge gets a 4 KB trimmed window and routinely returns 0/0/0 when the canonical summary line falls outside that window, scoring real passes as FAIL. Verified on rust-acronym across all 4 models in the last matrix: saved `src/lib.rs` passed 10/10 against `cargo test` on the host but scored FAIL. Also writes `~/.smooth/bench-runs/<id>/<task>/.smooth-score-forensic/{combined.txt,summary.json}` on every score attempt so failures are forensically diagnosable. Pearl th-086f0f.
