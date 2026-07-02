---
'@smooai/smooth': patch
---

smooth-bench: tag upstream LLM-gateway failures as `upstream-error` instead of scoring them as task failures. Mid-stream drops from llm.smoo.ai (`connection closed before message completed`, `client error (SendRequest)`, 5xx from the gateway) previously scored as FAIL, making gateway flakiness indistinguishable from real smooth bugs. Runs matching the reviewable signature table (`sweep::UPSTREAM_ERROR_SIGNATURES`) are now counted separately in the Score (`tasks_upstream_error`), excluded from the real-attempt pass rate alongside `inconclusive`, and surfaced with a count in the rendered summary and per-task stream output.
